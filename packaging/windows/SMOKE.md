# Windows Smoke Checklist

This is the manual, interactive smoke test for the Windows build. It covers the
behavior that cannot be verified headlessly: the tray, the global hotkeys, and
paste delivery into a focused application.

Run these steps on a Windows machine after building the distribution with
`packaging\windows\build-dist.ps1`. Verify both emitted SHA-256 files, install
the NSIS `.exe` on a clean test account, and repeat the core checks with the
portable `dist\codex-voice-windows-x64.zip` build.

## Preconditions

- Codex auth is present and valid (`codex-voice.exe doctor codex-auth` succeeds).
- A working microphone and audio output device are available.
- Have a plain, non-elevated text editor open as the paste target (Notepad is
  fine). Do not use an application running as Administrator for the paste test
  (see the UIPI note below).

## Checklist

1. **Launch and tray icon.** Run `codex-voice.exe run` from a terminal. Confirm
   the Codex Voice tray icon appears in the notification area.

   For the NSIS path, also confirm a missing WebView2 runtime is bootstrapped
   and the installer is scoped to the current user.

2. **Tray menu.** Right-click the tray icon. Confirm the menu opens and each
   item behaves as expected (status is shown, log/quit items work). Confirm that
   choosing quit exits the process cleanly.

3. **Hold-to-dictate (Control-M).** Relaunch if you quit. Focus the text editor,
   hold Control-M, speak a short phrase, then release. Confirm the transcribed
   text is pasted into the editor at the cursor.

4. **Speak selection (Win-F6).** Select some text in any application, then press
   Win-F6. Confirm the main window opens (or comes to the foreground) with the
   selected text prefilled in the editor and speech generation starting
   automatically; confirm the generated audio plays through the audio output.

5. **Tray status transitions.** Watch the tray icon/tooltip while dictating.
   Confirm it reflects the idle -> listening -> working states and returns to
   idle when done.

## UIPI caveat (paste delivery)

Paste is delivered with `SendInput`. Windows User Interface Privilege Isolation
(UIPI) blocks synthetic input from a lower-integrity process into a
higher-integrity (elevated) window. If the focused application is running as
Administrator while `codex-voice.exe` is not, the paste silently fails.

Always run the paste test against a normal, non-elevated editor. If a paste must
land in an elevated application, run `codex-voice.exe` at the same integrity
level as that application.
