# LEIO Code Support

This file is the editable source for the public support notice served by the Apps SDK runtime at `/support`.

For public / store submission, set the `LEIO_APPS_SDK_*` legal env vars listed in README so `/health` reports `legal.configured: true`.

## What Users Can Contact Support About

- trouble authenticating into the app;
- problems connecting the app in ChatGPT;
- repository inspection failures or incorrect results;
- suspected security issues or abuse;
- privacy requests routed to the privacy contact.

## What the Reviewer Needs

If OpenAI review requires authentication, provide a demo account that:

- works outside your internal network;
- does not require MFA or one-time SMS/email codes;
- has enough repository access to complete the documented test prompts.

## Recommended Public Fields

- support email;
- security email;
- privacy email;
- support hours;
- company URL.

## Recommended Response Policy

- acknowledge normal support requests within one business day;
- acknowledge security reports as quickly as possible;
- keep the public page aligned with the actual support workflow.
