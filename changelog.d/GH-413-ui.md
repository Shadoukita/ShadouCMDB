### Changed: Your own user page in Administration links to My account for your password

The user page under **Administration → Users** no longer offers **Reset password** on your own
account, matching the API, which now refuses an admin reset of your own password or two-factor
authentication ([GH#413]). The panel instead links to **My account**, where you change your own
password with your current one. Resetting another user's password is unchanged.

[GH#413]: https://github.com/Shadoukita/ShadouCMDB/issues/413
