# Privacy Policy

Sonora does not require an account and does not operate a service that stores users' music libraries, listening history, streaming credentials, or personal profiles.

Sonora sends no telemetry, analytics, or installation reports anywhere.

## Streaming and third-party services

When you use Deezer, lyrics providers, or other online integrations, Sonora communicates with those third-party services as necessary to provide the requested functionality.

Data handled by those services is subject to their respective privacy policies and terms.

## Self-hosted servers

Subsonic/OpenSubsonic and Maloja servers are often self-hosted and reached over HTTPS with a self-signed certificate. If you choose "Trust a self-signed or otherwise invalid certificate" when connecting to one, Sonora skips certificate verification for that server's address only. The connection stays encrypted, but the certificate is not verified, so only enable it for a server you control.

## Authentication credentials

Authentication credentials used by Sonora are stored locally on the user's device.

Sonora does not transmit streaming-service credentials to Sonora-operated servers.

Credentials are sent only to the relevant third-party service when required for authentication or API requests.

## Local data

Sonora stores application data locally on the user's device, including settings, playback state, provider credentials, local-library information, and other application state required for the application to function correctly.

On Linux, the running process also restricts itself with a Landlock filesystem sandbox, so it can only write its own directories and the configured music folders and only read the system directories it needs.

## Logs

Sonora may write diagnostic logs locally on the user's device.

These logs are not uploaded to Sonora-operated servers.

Users may choose to share logs manually when reporting bugs or requesting support.

## Changes to this policy

Material changes to this privacy policy will be published in the Sonora source repository.

## Contact

Privacy-related questions may be submitted through the Sonora GitHub repository:

https://github.com/Sudo-Ivan/sonora
