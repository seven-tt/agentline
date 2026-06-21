# DingTalk Bot Setup Guide

[中文](setup-dingtalk.md)

> No coding required — just follow the steps.

---

## Step 1: Create an Internal App

Open the [DingTalk Open Platform](https://open.dingtalk.com/) and log in with an admin account.

Click "App Development" at the top → "Internal Apps" on the left → "Create App"

Fill in:
- App name: e.g. `AI Assistant`
- App description: e.g. `AI bot powered by Agentline`
- App icon: Upload an image
- Development mode: "Internal development"

Click "Confirm"

> 💡 DingTalk also offers a quick-create entry. See the [official tutorial](https://open.dingtalk.com/document/development/build-dingtalk-ai-employees).

## Step 2: Add Bot Capability

Go to the app detail page → Click "Bot" on the left → Click "Add"

Configure:
- Bot name: Can be the same as the app name
- Message receive mode: Select **"Stream Mode"** (recommended, no public IP required)
- Keep other settings as default

Click "Publish"

## Step 3: Save Your Credentials

On the app detail page → "Credentials & Basic Info":

Copy and save:
- **Client ID** (also called AppKey, e.g. `dingxxxxxxxx`)
- **Client Secret** (click to copy)

On the bot page, you'll also see:
- **Robot Code**: Copy and save (usually the same as Client ID)

## Step 4: Publish the App

Click "Version Management & Release" → "Create New Version" → Fill in version info → "Submit for Review" → Wait for admin approval.

Once approved, search for the bot name in DingTalk to use it.

## Step 5: Connect to Agentline

Open Agentline Dashboard → "IM" in the sidebar → DingTalk card:

- Enter `Client ID` and `Client Secret`
- Toggle the enable switch

Save and test by sending a message to the bot in DingTalk.

---

## Troubleshooting

| Symptom | Cause | Solution |
|---------|-------|----------|
| Bot doesn't reply | Message mode is not Stream | Check bot config, ensure Stream mode is selected |
| Can't find the bot | App not published / not approved | Check "Version Management & Release" status |
