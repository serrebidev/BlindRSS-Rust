# Credits

This is the Rust port of [BlindRSS](https://github.com/serrebidev/BlindRSS). The Python app remains the product until this port reaches feature parity. It opens the existing Python `rss.db` and `config.json` in place, so there is no re-setup and no destructive migration.

Third-party projects it uses:

- wxDragon 0.9.24 (wxWidgets) for the interface; the workspace shape follows BlindFB.
- yt-dlp stays a managed executable; libVLC is the playback parity target.
- The NVDA controller client asset (`app/assets/nvdaControllerClient-LICENSE.txt`) for screen-reader detection; it stays under its own LGPL license.

Questions and bug reports are welcome in the [SerrebiProjects Telegram group](https://t.me/SerrebiProjects). More serrebi projects: [projects.serrebiradio.com](https://projects.serrebiradio.com).
