### Remote access notes

The user can open normal intranet pages, but VPN login keeps looping back to the approval screen. The client displays "MFA token expired" and then asks for 2FA again.

The root cause is phone time drift after the user disabled automatic date and time. The authenticator code is generated with the wrong clock, so the VPN gateway rejects every one-time password.

Fix steps: sync phone time automatically, remove the old authenticator binding, ask the admin to reset the MFA seed, then log in to VPN again and approve the push notification.
