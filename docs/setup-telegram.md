# Telegram 机器人接入教程

[English](setup-telegram.en.md)

> 不需要写代码，跟着点就行。Telegram 机器人申请是所有平台里最简单的。

---

## 步骤 1：找 BotFather 创建机器人

在 Telegram 搜索 [@BotFather](https://t.me/BotFather)，给它发消息：

```
/newbot
```

BotFather 会问你两个问题：
1. **机器人显示名称**：随便起，如 `My AI Assistant`
2. **机器人用户名**：必须以 `bot` 结尾，如 `my_ai_assistant_bot`

创建成功后，BotFather 会返回一段话，里面有一行：

```
Use this token to access the HTTP API:
123456789:ABCdefGHIjklMNOpqrsTUVwxyz
```

这串 `123456789:ABCdefGHIjklMNOpqrsTUVwxyz` 就是你的 **Bot Token**，复制保存。

> ⚠️ Bot Token 相当于机器人的密码，不要发到公开群里。如果泄露了，可以在 BotFather 用 `/revoke` 重新生成。

## 步骤 2：获取你的 User ID（可选，用于白名单）

如果你想限制只有自己能和机器人聊天，需要知道你的 Telegram User ID。

搜索 [@userinfobot](https://t.me/userinfobot)，给它发任意消息，它会回复你的 User ID（一串纯数字）。

## 步骤 3：填入 Agentline

打开 Agentline Dashboard → 左侧「IM」菜单 → Telegram 卡片：

- **Bot Token**：填入上面拿到的 Token
- **API Base URL**：留空（默认使用官方 API）；如果你在国内需要用代理，填入代理地址
- **允许的用户**：填入你的 User ID（每行一个）；留空表示允许所有人

打开启用开关，保存即可。

## 步骤 4：测试

在 Telegram 搜索你刚创建的机器人用户名，发送 `你好`，应该收到 AI 回复。

---

## 常见问题

| 现象 | 原因 | 解决办法 |
|------|------|----------|
| 机器人完全没反应 | Bot Token 填错了 | 重新从 BotFather 复制 Token |
| 机器人回复很慢 | 网络问题（国内直连 Telegram API 不稳定） | 配置 API Base URL 使用代理 |
| 只想自己用，别人也能发消息 | 没配置白名单 | 在「允许的用户」填入你的 User ID |

## 附录：其他有用的 BotFather 命令

| 命令 | 作用 |
|------|------|
| `/mybots` | 查看你创建的所有机器人 |
| `/setname` | 修改机器人显示名称 |
| `/setdescription` | 修改机器人简介 |
| `/setuserpic` | 修改机器人头像 |
| `/revoke` | 重新生成 Bot Token |
| `/deletebot` | 删除机器人 |
