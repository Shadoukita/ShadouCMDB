### Fixed: sign-in after a session ended on the two-factor set-up screen returns to the requested page

When a profile requires two-factor authentication, a user who signed in with the password alone
waits on the authenticator set-up screen. If their session ended there (for example because the
authenticator was confirmed in another browser), signing in again with a code led to **My account**
instead of the page they originally asked for. Sign-in now returns them to that page.
