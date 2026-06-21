# Feishu (Lark) Bot Setup Guide

[中文](setup-feishu.md)

> No coding required — just follow the steps.

---

## Step 1: Quick Create

Feishu Open Platform provides a "Quick Bot" entry that pre-configures bot capabilities, messaging permissions, and event subscriptions.

Open: https://open.feishu.cn/document/develop-an-echo-bot/introduction?from=op_develop_app

Or go to the [Feishu Open Platform Console](https://open.feishu.cn/app) → "Build App" → "Quick Bot"

The system will create an enterprise internal app with bot capabilities. You just need to:
- Name your bot (e.g. `AI Assistant`)
- Upload an icon
- Click "Create"

## Step 2: Save Your Credentials

After creation, go to the app dashboard. Click "Credentials & Basic Info" in the left menu:

Copy and save these two fields:
- **App ID** (e.g. `cli_xxxxxxxx`)
- **App Secret** (click the eye icon to reveal)

> ⚠️ App Secret is only shown once. If you refresh the page, click "Refresh" to regenerate it. Treat it like a password — never share it publicly.

## Step 3: Verify Auto-Configuration

The quick create process has already configured:

- ✅ Capability: Bot
- ✅ Permissions: Read private messages, send messages as bot, receive group @ mentions
- ✅ Event subscription: Long-connection mode, subscribed to `im.message.receive_v1`

## Step 4: Publish the App

Click "Version Management & Release" → "Create Version"

Fill in:
- Version: `1.0.0`
- Release notes: `Initial version`
- Availability: "All employees" or "Partial availability"

Click "Save" → "Submit for Review" → Have your admin approve it in the Feishu Admin Console.

Once approved, search for the bot name in Feishu to find it.

## Step 5: Connect to Agentline

Open Agentline Dashboard → "IM" in the sidebar → Feishu card:

- Enter `App ID` and `App Secret`
- Toggle the enable switch

Save and test by sending a message to the bot in Feishu.

---

## Troubleshooting

| Symptom | Cause | Solution |
|---------|-------|----------|
| Bot doesn't respond at all | App not published / not approved | Check "Version Management & Release" status |
| Bot doesn't respond in group | Bot not added to group | Add the bot to the group members |

## Appendix: When Do You Need Extra Permissions?

| Requirement | Permission to Add |
|-------------|-------------------|
| Auto-reply in groups without @ | `im:message.group_msg` |
| Get user employee ID | `contact:user.employee_id:readonly` |
| Read docs/sheets/wiki | `docs:document.content:readonly`, `wiki:wiki:readonly` |

If you don't need these, you don't need to touch the permission settings at all.
