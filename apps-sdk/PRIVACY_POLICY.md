# LEIO Code Privacy Policy

This file is the editable source for the public privacy notice served by the Apps SDK runtime at `/privacy`.

For public / store submission, set the `LEIO_APPS_SDK_*` legal env vars (README) and verify the rendered `/privacy` page matches your retention, subprocessors, and support channels.

## Summary

`LEIO Code` is a read-only repository inspection app. It helps users inspect repository structure, symbols, deployment contracts, and operational drift. It does not push code, mutate repositories, send messages to third parties, or create public side effects.

## Data We Process

The service may process:

- OAuth and identity data needed to authenticate the user, such as user ID, email, and basic profile claims from the configured identity provider.
- Tool inputs submitted by the user inside ChatGPT.
- Repository content that the user or deployment operator explicitly configured the app to inspect.
- Tool outputs returned to the user.
- Operational and security telemetry needed to run, secure, and troubleshoot the service.

## How We Use Data

We use this data to:

- authenticate and authorize access to the app;
- execute repository inspection and explanation requests;
- secure the service, detect abuse, and investigate incidents;
- operate, monitor, and improve reliability of the deployment;
- comply with legal obligations.

## Data Sharing

Data may be processed by infrastructure and identity providers that host the app, its authentication layer, and related operational telemetry. Replace this section with the exact production subprocessor list before public launch.

## Retention

Repository queries, auth claims, and operational logs should be retained only as long as needed to operate, secure, and troubleshoot the service. Replace this section with the actual retention schedule used in production before public launch.

## Security

The app is intended to expose read-only tooling. Access is authenticated through the configured identity provider and authorized by token validation on each protected request.

## User Rights and Requests

Requests about access, correction, deletion, or privacy should go to the privacy contact published on the public policy page.

## Contact

The public deployment should expose:

- a company URL;
- a support contact;
- a privacy contact;
- a security contact.

Do not submit to the ChatGPT store until `/health` shows `legal.configured: true` and OAuth works on the public URL.
