# Tool status (verified)

`docs/tools.md` (generated) lists every tool. This page records whether each one **works**, from a
functional audit on 2026-10-16 (Windows 11, real inputs), later live runs, and the fixes that
followed.

Legend: **Verified** = ran on real input with plausible, spot-checked output · **Partial** = runs
but output is incomplete in a known way · **Fixed** = was broken or partial in the audit, fixed
and re-checked · **Unverified** = implemented, not runnable here (needs a device, engine or OS
we don't have); source reviewed · **New** = added since the audit, unit-tested and smoke-tested.

## Summary by toolset

| Toolset | Verified | Partial | Fixed | Unverified | Notes |
|---|---|---|---|---|---|
| Core (session, evidence, triage, overview, reports) | 19 | 3 | 7 | 0 | evidence bundles, core_session_show, report, core_target_open, triage fixed |
| Native (listings, functions, xrefs, pseudo-C, bytes, annotations) | 43 | 3 | 2 | 0 | see partials below |
| Browser / web | 10 | 1 | 2 | 0 | +3 new website tools (open/capture/close) |
| JavaScript / Electron | 7 | 1 | 0 | 2 | runtime attach needs a debuggable app |
| Application graphs, comparison, reconstruction | 12 | 3 | 0 | 2 | |
| Managed (.NET) | 7 | 0 | 0 | 0 | no C# decompiler (IL + structure only) |
| Java | 6 | 0 | 0 | 0 | `java_vineflower_decompile` verified on a real JAR (Commons IO 2.16.1, OpenJDK 21.0.12.1, Linux) |
| Android (static) | 9 | 0 | 0 | 4 | JADX/Apktool/JEB paths unverified |
| Android device (adb) | 25 | 0 | 0 | 0 | all run on an emulator (below, 2026-11-10 and 2026-11-13); none on a physical phone |
| Apple | 8 | 0 | 0 | 6 | synthetic Mach-O/plist only; runtime tools macOS-only, except `runtime_ui_read` and `runtime_ui_walkthrough_record`, which also read and drive Windows apps (UI Automation; tested against a WinForms window) |
| Formats, firmware, database | 11 | 0 | 0 | 1 | |
| Runtime (process capture, native calls/UI) | 0 | 0 | 0 | 5 | run foreign code; not executed in the audit |
| Frida | 0 | 0 | 0 | 9 | Frida not installed; parts of session handling are simulated in source |
| Added since the audit | — | — | — | — | 15 new: `core_text_read`, `native_bytes_find`, `native_rust_binary_show`, `js_electron_fuses_show`, `formats_wasm_compare`, `packets_capture_show`, `packets_fields_show`, `engines_tshark_check`, `packets_inventory_show`, `native_objdump_run`, `native_rizin_command_run`, `native_gdb_session_begin`, `native_gdb_command_run`, `native_gdb_session_show`, `native_gdb_session_end` |

## Real-binary run (2026-10-21)

The built-in engine (Clean room) on four real, stripped programs: a Windows system program (MSVC,
x86-64 PE, 340 KB), a Rust command-line tool (x86-64 PE, 1.1 MB), a Rust shared library (x86-64
ELF, 485 KB) and a Go command-line tool (x86-64 PE, 43 MB). Before → after this round:

| Program | Functions | Named functions | Instructions | Strings | Time |
|---|---|---|---|---|---|
| MSVC system program | 872 → 872 | 43 → 43 | 60,358 → 58,637 | 872 → 876 | 0.1 s |
| Rust tool (PE) | 1,317 → 1,318 | 30 → 30 | 2,307,416 → 222,201 | 1,295 → 1,609 | 2.0 s → 0.2 s |
| Rust library (ELF) | 571 → 561 | 17 → 17 | 385,399 → 90,680 | 144 → 298 | 0.3 s → 0.1 s |
| Go tool | 36,536 → 43,687 | 1 → 43,686 | 3,665,388 → 3,708,492 | 127,701 → 134,759 | 3.2 s → 3.1 s |

Fixed:
- **Go function names.** Go keeps every function's name in its function table even when
  stripped; those entries now seed and name functions (`name_source: go_pclntab`). It also found
  7,726 functions nothing else reached.
- **Functions running into their neighbours.** A switch table with no bound before its jump was
  read 256 entries deep, and the entries past its end pulled other functions' code into the
  switch's function (up to the 60,000-instruction cap; the Rust tool's total was ten times its
  real size). Table entries now stay between the function starts around the jump, a branch to
  another function's entry is a tail call, and functions scanned before a later-found entry was
  known are scanned again once every entry is known.
- **Packed strings.** Rust and Go store text back to back without terminators, so two literals
  read as one. Where code points into the middle of a string, a new one now starts there.

## Still partial (known gaps)

- `native_function_pseudocode`, `native_function_study`: imports outside the argument-count table
  (`disasm::arity`) show only the arguments set in the calling block.
- `native_strings_browse`: most strings in a PE come from resources; `section` limits the list to one section.
- `web_page_show` with `launch` but no open website still sees a blank page (by design; open a
  site with `web_site_open` first).
- `web_site_open` keeps its browser for one session, so from the CLI, where each command is a
  session of its own, a later `web-site-capture` finds no site (it says so). Run them together in
  an MCP client (`craft-anything-cli mcp`), the app or `rebuild` (#153).
- `web_coverage_measure`: coverage on a page with no scripts reports 0 of 0.

## Engines and how they're used

- Packet tools use our built-in pcap/pcapng decoder (`packets_capture_show` for an overview,
  `packets_fields_show` for per-packet fields); `packets_inventory_show` uses tshark when it's
  installed.
- Cutter sessions (`native_cutter_sessions_list`, `native_cutter_command_run`): **New**. The person
  shares a session with `=h& 9090` in Cutter's console; commands are read-only (the
  native_rizin_command_run allow-list) and only 127.0.0.1 is contacted. Tested against a stand-in
  HTTP server; **Unverified** against a real Cutter.
- Radio decoding (`radio_recording_decode`, `radio_decoders_list`): **New**, through rtl_433 run as
  a program. Checked with rtl_433 25.12 on Windows against an Acurite rain gauge recording from
  rtl_433's public test samples.
- Rizin debug sessions (`native_rizin_session_begin`, `_command_run`, `_show`, `_end`): **New**.
  Start, registers, error lines and close checked with Rizin 0.9.1 on Windows; the debugged
  program's own console output isn't captured.
- Vineflower (`java_vineflower_decompile`): **Verified** on Linux x86-64 with OpenJDK 21.0.12.1
  and the pinned Vineflower 1.12.0, against Apache Commons IO 2.16.1 (497 KB, 374 entries):
  253 source files, exit 0. `org.apache.commons.io.FileUtils` came back at 1,350 lines, and the
  class and its file-reading methods are present. Not checked: Windows, macOS, a `.class` input,
  or a second JAR.
- Android device tools on an emulator (2026-11-10, Windows 11, Android 17 / API 37, x86_64,
  through the CLI): `adb_devices_find`, `adb_device_show` (553 properties), `adb_apps_list` (260
  packages; 0 third-party), `adb_apps_find`, `adb_app_show`, `adb_log_read`,
  `adb_running_processes_list` (331), `adb_folder_list`, `adb_device_features_list` (98),
  `adb_system_services_list` (324), `adb_display_show`, `adb_window_show` (the focused window
  named the app just launched), `adb_setting_get`, `adb_screenshot_take` (a 1080 x 2400 PNG),
  `adb_app_download`, `adb_file_upload` and `adb_file_download` (the file came back with the same
  text), `adb_app_launch`, `adb_activity_open`, `adb_app_force_stop`. **Verified**. `adb_log_read`
  with `count` 200 returned 270 lines: logcat counts entries, and an entry can span lines.
  **Unverified**: every tool on a physical phone.
- Install, remove and bug report (`adb_app_install`, `adb_app_remove`, `adb_bugreport_save`):
  **Verified**. Run on 2026-11-13 on the Android 17 (API 37, x86_64) emulator `Medium_Phone`, in
  Deep mode through the CLI, with a Flutter app built on this computer (`com.example.cam_counter`,
  profile APK): install said "Success" and `pm list packages` listed it; remove said "Success" and
  the package was gone; the bug report was one 5.5 MB zip made in 22 s, in a folder the tool
  created. The install's params showed `replace` true without being passed (the default). Not
  checked: Clean room (all calls were Deep), a physical phone, installing an `.aab`.
- App state on device (`adb_app_state_show`): **New**. Run on the emulator above in Deep mode:
  Settings' home activity on screen, 9 tasks, Settings running (one pid) in tasks 7 and 13, 149
  permissions asked for and 208 held; YouTube installed and not running, its denied run-time
  permissions the same as `dumpsys package` shows. In Clean room it was refused. Not checked: an
  older Android's listing, several displays, a physical phone.
- Compiled Android code (`android_odex_show`): **New**. System files pulled from the emulator
  (Android 17): `services.odex` and `boot.oat` read as OAT 275 with compiler filter
  `speed-profile`, an app's `.odex` with `verify`, and their `.vdex` files as VDEX 027 covering
  3 and 1 DEX files with no bytecode inside. Run again on 2026-11-13 on a system app's files
  (`BasicDreams`): the `.odex` as OAT 275 (filter `verify`, 1 DEX header, 17,240 bytes) and the
  `.vdex` as VDEX 027 (1 DEX file covered, none with its bytecode inside, 248 bytes). An app's own `oat/` folder can't be read without
  root. With the OAT header read by version (same day, same files): the `.odex` gives instruction
  set x86_64, 1 DEX record (`/system/app/BasicDreams/BasicDreams.apk`, DEX 039, checksum equal to
  the `.vdex`'s) and 13 settings from its key-value store. Only OAT 275 and VDEX 027 were checked on
  real files; the other OAT layouts (138–259) and VDEX 019/021 come from the runtime's published
  sources only, and say so. A `.art` image is named with its version; nothing else in it is read.
- App frameworks (`android_frameworks_find`): **New**. On base APKs pulled from the emulator:
  YouTube and Chrome come out as Android Views (layouts found through the resource table, as
  their `res/` paths are shortened), Messages and Settings as Jetpack Compose (strong), with
  WebView use as a weak sign. Kotlin was found only in Settings: R8 strips its marker classes in
  the others. A real Flutter debug APK (`flutter create` + `flutter build apk --debug`) came out
  as Flutter (strong); so did a profile APK on 2026-11-13 (strong, observed, all 149 files read,
  `libflutter.so` for three ABIs, `flutter_assets/`, `libapp.so`), with Kotlin strong and Android
  Views and WebView as weaker signs that Flutter's own embedding classes bring in. Not checked: a
  real React Native, .NET, Unity or Capacitor app.
- Android app in several files (`android_bundle_show`): **New**. Chrome's six split APKs, pulled
  from the emulator and zipped as `.apks`: package, version code, SDK 32–36 and compile SDK 37,
  the base (x86_64 code, 73 components), four feature splits and the `config.en` language split.
  Also a real `.aab` on 2026-11-13 (Flutter `build appbundle`, 42 MB): package, version 1 /
  1.0.0, SDK 24-36, one base module with 3 components (2 exported), 1 DEX file, 8 assets, ABIs
  arm64-v8a, armeabi-v7a and x86_64, no feature or configuration splits. Not checked: a real
  `.xapk`, a multi-module bundle.
- Package and resources of a bundle (`android_package_show`, `android_resources_read`): **New**.
  Run on that `.aab` on 2026-11-13 in Deep mode. `android_resources_read` gave 1 package and 104
  values in the default locale. `android_package_show` read the manifest (SDK 24-36, 1
  permission, 3 components), the DEX file (576 classes, 3,921 methods, checksum fine) and a v1
  signature (verified, debug certificate), but its summary said "native code for no
  ABI" and 0 assets where `android_bundle_show` finds 3 ABIs and 8 assets, and it warned that
  signing is not read although it read it. Fixed and run again the same day on a release bundle
  of the same app: 3 ABIs (two libraries each) and 8 assets, as `android_bundle_show` says, and
  the warning names only the other modules. `android_resources_read` on that bundle and on the
  same app's release APK agree on the qualifiers (`default`, `night`, `mdpi` to `xxxhdpi`) and on
  each style's parent, item names and kind of value; the bundle shows names where the APK has
  ids, and lists the resources the APK's shrinking removed. Not checked on a real file: arrays
  and plurals (the app has none), other qualifiers, a bundle with feature modules. Not run in
  Clean room.
- Replay a walkthrough (`adb_walkthrough_replay`): **New**. On the emulator above, a 3-step walk
  in Settings (swipe, wait, back) recorded, then replayed from the same screen: 0 of 3 captures
  differ (same app and controls; pictures not byte-identical after the first). Replayed from the
  home screen: 2 of 3 differ (another app, 25 controls added, 13–14 removed). Not checked: a
  walk with kept-out text, a rebuild, another device, a physical phone.
  Pictures are now compared pixel by pixel below the status bar: replayed from the same screen,
  0% and 0.4% of pixels differ before and after, 9% after the swipe (the fling stopped elsewhere).
- Record a walkthrough (`adb_walkthrough_record`): **New**. Two live walks on the emulator above:
  Settings search (tap, type, two backs: 5 captures, the app on screen changed and came back),
  and Add network (7 steps, 8 captures) with a password typed without `secret` into the field
  `id/password`: it was kept out automatically, and no file in the folder nor the evidence held it,
  also after "Show password". Not checked: a capture that fails (a screen that never settles), a
  refused step, a physical phone.
- Tap, type and swipe (`adb_input_send`): **New**. Run on the emulator above: a tap on Settings'
  search box, then "wi fi" typed (the field then held exactly that, space included); back and
  home keys; Add network opened, a name typed, the security list opened and an entry picked, a
  password typed and "Show password" tapped, each step's result read with `adb_ui_show`. Without
  consent it was refused. A swipe and a long press were sent and taken. Not checked: what a swipe
  or long press did on screen, a step the device refuses, a physical phone.
- Controls on the screen (`adb_ui_show`): **New**. Run on the emulator above against Android 17's
  Settings: 78 controls, 13 interactive; `interactive_only` with `text_contains` "network" returned
  the one row a tap opens, labelled from its two texts, with the row's bounds. Before labels the
  same call found nothing, because the words sit in a child that takes no tap. A Wi-Fi password
  typed into Add network: Android's own dump held it while masked and, after "Show password",
  marked the field `password="false"`; the tool returned it in that second case until typed text
  was hidden for every editable field. Rechecked live: neither case returns it. Not checked live:
  a WebView, a Compose text field, a screen that never settles, or a physical phone.
- Virtual devices (`adb_emulators_list`, `adb_emulator_start`, `adb_emulator_stop`): **New**. Run
  on Windows 11 with emulator 36.6.11: the list named the device and, once started, its serial
  and finished boot; start (`no_window`) returned after a 30 s boot with serial `emulator-5554`,
  and the emulator outlived the CLI; a second start reported it already running; stop without
  consent and stop of a phone's serial were refused; after stop adb listed no device and no
  emulator process was left. Not checked: macOS, Linux, a windowed start, `cold_boot`, a boot
  that outlasts the wait, or an emulator that fails to start (fixtures cover the refusals only).
- Default device (`adb_default_device_set`): **New**. Run on Windows 11 on 2026-11-13 from the
  CLI: setting `Medium_Phone` wrote `android-devices.json` (version 1, `{"avd": …}`), Play then
  used it (`device_how: default`), `adb_devices_find` showed it, a name with a space was refused
  and `clear` removed it. Not checked: a phone's serial, or the app.
- Play on Android (`project_play`, `on: "android"`): **New**. Run on Windows 11 on 2026-11-13
  from the CLI (output to a file, not a pipe) with a real Views debug APK in a project's app/:
  with nothing online it started `Medium_Phone` with a window, installed and started the app in
  27 s and returned, the app's activity in front; run again it reinstalled on the only device
  online; with `device: Medium_Phone` and `no_window` (what `InstallCheck` sends) it used the
  running emulator; without consent and with an unknown player it was refused before anything
  ran. `adb_devices_find` with `for_check` named the virtual device to start, then the running
  one. Not checked: several devices, the restart of an emulator started without a window, a
  failed install, a phone, the desktop app (no button yet), or a rebuild run's `InstallCheck`.
- Android SDK check (`engines_android_sdk_check`): **New**. Run on Windows 11 against a real SDK
  found through the adb on PATH: 4 build tools, 4 platforms, 2 NDKs, command-line tools, emulator,
  one system image and one virtual device, each matching the folders and `config.ini` on disk.
  Not checked: macOS, Linux, an SDK with no `source.properties`, or a device made on another
  computer (covered by fixtures only).
- Also: Java (.class/JAR) tools, Solana sBPF, SQLite, investigation memory, the report tool,
  website open/capture, and the rebuild workflow.
