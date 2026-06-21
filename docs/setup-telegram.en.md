# Telegram Bot Setup Guide

[中文](setup-telegram.md)

> No coding required — Telegram has the simplest bot creation process of all platforms.

---

## Step 1: Create a Bot with BotFather

Search for [@BotFather](https://t.me/BotFather) in Telegram and send:

```
/newbot
```

BotFather will ask two questions:
1. **Display name**: Anything you like, e.g. `My AI Assistant`
2. **Username**: Must end with `bot`, e.g. `my_ai_assistant_bot`

After creation, BotFather will reply with a message containing:

```
Use this token to access the HTTP API:
123456789:ABCdefGHIjklMNOpqrsTUVwxyz
```

This `123456789:ABCdefGHIjklMNOpqrsTUVwxyz` is your **Bot Token** — copy and save it.

> ⚠️ Your Bot Token is like a password. Never share it publicly. If compromised, use `/revoke` in BotFather to regenerate it.

## Step 2: Get Your User ID (Optional, for Allowlist)

If you want to restrict the bot to only respond to you, you'll need your Telegram User ID.

Search for [@userinfobot](https://t.me/userinfobot) and send any message — it will reply with your numeric User ID.

## Step 3: Connect to Agentline

Open Agentline Dashboard → "IM" in the sidebar → Telegram card:

- **Bot Token**: Paste the token from Step 1
- **API Base URL**: Leave empty (uses official API by default); fill in a proxy URL if needed
- **Allowed Users**: Enter your User ID (one per line); leave empty to allow everyone

Toggle the enable switch and save.

## Step 4: Test

Search for your bot's username in Telegram, send `hello`, and you should receive an AI reply.

---

## Troubleshooting

| Symptom | Cause | Solution |
|---------|-------|----------|
| Bot doesn't respond at all | Bot Token is incorrect | Re-copy the token from BotFather |
| Bot responds very slowly | Network issues | Configure API Base URL with a proxy |
| Others can message my bot | No allowlist configured | Add your User ID to "Allowed Users" |

## Appendix: Useful BotFather Commands

| Command | Description |
|---------|-------------|
| `/mybots` | List all your bots |
| `/setname` | Change bot display name |
| `/setdescription` | Change bot description |
| `/setuserpic` | Change bot avatar |
| `/revoke` | Regenerate Bot Token |
| `/deletebot` | Delete a bot |
