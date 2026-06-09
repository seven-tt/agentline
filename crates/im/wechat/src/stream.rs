use base64::Engine;
use crate::error::{Error, Result};
use crate::http::HttpClient;
use crate::types::{
    AbortInfo, InitStreamReq, InitStreamResp, PieceItem, SyncStreamReq, SyncStreamResp,
};
use serde::Serialize;

const STREAM_BUSINESS_TYPE: i32 = 10;
const ABORT_TYPE_CLIENT: i32 = 1;

/// Payload encoded into `PieceItem.piece_data` (base64 JSON).
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum PiecePayload {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(rename = "stream_type")]
        stream_type: String,
    },
    #[serde(rename = "tool_calling")]
    ToolCalling {
        name: String,
        phase: String,
        #[serde(rename = "stream_type")]
        stream_type: String,
    },
}

/// Manages a single iLink uplink piece stream.
pub struct WeixinStreamSender {
    http: HttpClient,
    device_id: String,
    client_stream_id: String,
    stream_ticket: Option<String>,
    piece_seq: i32,
    ended: bool,
    /// Pieces from a previous failed call, kept for retry.
    pending_pieces: Vec<PieceItem>,
    seq_before_pending: i32,
}

impl WeixinStreamSender {
    pub fn new(http: HttpClient, device_id: String, client_stream_id: String) -> Self {
        Self {
            http,
            device_id,
            client_stream_id,
            stream_ticket: None,
            piece_seq: 0,
            ended: false,
            pending_pieces: Vec::new(),
            seq_before_pending: 0,
        }
    }

    /// Call `native_init_stream` to obtain a `stream_ticket`.
    pub async fn init(&mut self) -> Result<()> {
        let req = InitStreamReq {
            device_id: self.device_id.clone(),
            client_stream_id: self.client_stream_id.clone(),
            business_type: STREAM_BUSINESS_TYPE,
        };
        let resp: InitStreamResp = self
            .http
            .post_json("/ilink/bot/native_init_stream", &req)
            .await?;
        if let Some(br) = resp.base_response {
            if br.ret != 0 {
                return Err(Error::Api {
                    ret: br.ret,
                    msg: br.errmsg.unwrap_or_default(),
                });
            }
        }
        self.stream_ticket = resp.stream_ticket;
        tracing::debug!(stream_id = %self.client_stream_id, "stream initialized");
        Ok(())
    }

    pub fn client_stream_id(&self) -> &str {
        &self.client_stream_id
    }

    /// Send a single piece. `piece_seq` auto-increments from 1.
    /// Failed pieces are tracked in `pending_pieces` and retried on the next call.
    pub async fn send_piece(&mut self, payload: &PiecePayload) -> Result<()> {
        self.assert_ready()?;
        let seq_before = if self.pending_pieces.is_empty() {
            self.piece_seq
        } else {
            self.seq_before_pending
        };
        self.piece_seq += 1;
        let json = serde_json::to_string(payload)
            .map_err(|e| Error::other(format!("serialize piece: {e}")))?;
        let piece_data = base64::engine::general_purpose::STANDARD.encode(json);
        let new_piece = PieceItem {
            piece_seq: self.piece_seq,
            piece_data,
        };
        let pieces: Vec<PieceItem> = self
            .pending_pieces
            .iter()
            .cloned()
            .chain(std::iter::once(new_piece))
            .collect();

        let req = SyncStreamReq {
            device_id: self.device_id.clone(),
            client_stream_id: self.client_stream_id.clone(),
            business_type: STREAM_BUSINESS_TYPE,
            up_piece_list: pieces.clone(),
            end_up_piece_seq: 0,
            abort_info: None,
        };
        match self.http.post_json::<_, SyncStreamResp>("/ilink/bot/sync_stream", &req).await {
            Ok(resp) => {
                self.check_abort(&resp)?;
                let retried = pieces.len().saturating_sub(1);
                self.pending_pieces.clear();
                self.seq_before_pending = 0;
                tracing::debug!(seq = self.piece_seq, retried, "send_piece ok");
                Ok(())
            }
            Err(e) => {
                // Roll back: save all pieces for retry and restore piece_seq.
                self.pending_pieces = pieces;
                self.seq_before_pending = seq_before;
                self.piece_seq = seq_before;
                tracing::warn!(
                    stream_id = %self.client_stream_id,
                    error = %e,
                    "send_piece failed — pieces kept for retry"
                );
                Err(e)
            }
        }
    }

    /// Signal that the stream has ended.
    pub async fn end(&mut self) -> Result<()> {
        self.assert_ready()?;
        self.piece_seq += 1;
        let final_json = serde_json::json!({ "type": "text", "text": "", "stream_type": "text" }).to_string();
        let piece_data = base64::engine::general_purpose::STANDARD.encode(final_json);
        let final_piece = PieceItem {
            piece_seq: self.piece_seq,
            piece_data,
        };
        let pieces: Vec<PieceItem> = self
            .pending_pieces
            .iter()
            .cloned()
            .chain(std::iter::once(final_piece))
            .collect();

        let req = SyncStreamReq {
            device_id: self.device_id.clone(),
            client_stream_id: self.client_stream_id.clone(),
            business_type: STREAM_BUSINESS_TYPE,
            up_piece_list: pieces,
            end_up_piece_seq: self.piece_seq,
            abort_info: None,
        };
        self.http
            .post_json::<_, serde_json::Value>("/ilink/bot/sync_stream", &req)
            .await?;
        self.pending_pieces.clear();
        self.seq_before_pending = 0;
        self.ended = true;
        tracing::debug!(stream_id = %self.client_stream_id, end_seq = self.piece_seq, "stream ended");
        Ok(())
    }

    /// Send a client-side abort signal.
    pub async fn abort(&mut self, reason: &str) -> Result<()> {
        self.assert_ready()?;
        self.ended = true;
        let req = SyncStreamReq {
            device_id: self.device_id.clone(),
            client_stream_id: self.client_stream_id.clone(),
            business_type: STREAM_BUSINESS_TYPE,
            up_piece_list: Vec::new(),
            end_up_piece_seq: self.piece_seq.max(1),
            abort_info: Some(AbortInfo {
                abort_type: ABORT_TYPE_CLIENT,
                abort_detail_error_code: 0,
                abort_detail_error_msg: reason.to_string(),
            }),
        };
        self.http
            .post_json::<_, serde_json::Value>("/ilink/bot/sync_stream", &req)
            .await?;
        tracing::debug!(stream_id = %self.client_stream_id, reason, "stream aborted");
        Ok(())
    }

    pub fn ticket(&self) -> Option<&str> {
        self.stream_ticket.as_deref()
    }

    pub fn is_ended(&self) -> bool {
        self.ended
    }

    fn assert_ready(&self) -> Result<()> {
        if self.stream_ticket.is_none() {
            return Err(Error::other(
                "WeixinStreamSender: not initialized — call init() first",
            ));
        }
        if self.ended {
            return Err(Error::other("WeixinStreamSender: stream already ended"));
        }
        Ok(())
    }

    fn check_abort(&self, resp: &SyncStreamResp) -> Result<()> {
        if let Some(ref info) = resp.abort_info {
            if info.abort_type != 0 {
                return Err(Error::other(format!(
                    "Stream aborted: type={} code={} msg={}",
                    info.abort_type,
                    info.abort_detail_error_code,
                    info.abort_detail_error_msg.as_deref().unwrap_or("")
                )));
            }
        }
        if let Some(ref br) = resp.base_response {
            if br.ret != 0 {
                return Err(Error::Api {
                    ret: br.ret,
                    msg: br.errmsg.clone().unwrap_or_default(),
                });
            }
        }
        Ok(())
    }
}
