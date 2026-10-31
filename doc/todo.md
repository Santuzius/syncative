# To do

- **Log lines without a length limit:** The folder logs (200 lines each, `MAX_LOG_LINES`) and the captured log (4096 lines, `RING_CAPACITY` in src/infrastructure/stderr_capture.rs) are bounded in lines but not in line length. In practice lines stay at a few hundred characters; cutting them at about 2000 would bound the memory for good.
- **AppStream metainfo:** ship `usr/share/metainfo/io.github.santuzius.syncative.metainfo.xml` in the AppImage with screenshots from `doc/img/`, then `/retest` the AppImageHub PR ([release.md](packaging/release.md#appimagehub)).
- **Tray icon appearing later:** Without a system tray at start, Syncative opens its window. Check whether it should add the tray icon once a StatusNotifierWatcher appears later (e.g. the GNOME AppIndicator extension enabled while it runs).
- **MEGA:** requested in issue 2, not decided yet; research and plan in [mega.md](providers/mega.md).
