# Soundpad for macOS

**Play sounds into Discord, Zoom, Teams and games, and hear them yourself, with one key press.**

Soundpad is a free soundboard for Mac. Add your sound clips and music, give them keyboard
shortcuts, and play them in voice chats as if they came from your microphone. Your friends
hear the sounds *and* your voice.

[**⬇ Download the latest version**](https://github.com/bynarig/soundpad/releases/latest) · [Website](https://bynarig.github.io/soundpad/)

Requires macOS 26 or later. Works on both Apple silicon and Intel Macs.

---

## What it can do

- 🎹 **Keyboard shortcuts for every sound.** They work even when Soundpad is in the background,
  for example while you are in a game.
- 🎙 **Its own virtual microphone.** Other apps can "hear" Soundpad. No extra software to install.
- 🗣 **Your voice goes through too.** Soundpad mixes your real microphone in with the sounds.
- 🔊 **Separate volume** for what you hear and what others hear.
- 📂 **Playlists.** Drag and drop files to sort your sounds into lists.
- ⏯ **Music player controls.** Play, pause, next, previous, jump ±10 seconds, click anywhere
  on the timeline, repeat one sound or play a whole list.
- 🎵 **Plays almost any audio file:** MP3, WAV, FLAC, OGG, AAC/M4A, AIFF and CAF.
  Long songs and hour-long mixes play without using much memory.
- 🌍 **40 languages,** including English, Ukrainian, every EU language, Chinese, Japanese,
  Arabic and Hindi. Soundpad uses your Mac's language automatically.
- 🔄 **Updates itself** when a new version comes out.

---

## Install

1. [Download **Soundpad-x.y.z.zip**](https://github.com/bynarig/soundpad/releases/latest)
   from the latest release.
2. Double-click the zip to unzip it, then drag **Soundpad** into your **Applications** folder.
3. **The first time only:** open Soundpad. macOS says *"Soundpad" Not Opened* because
   Soundpad is a free app that isn't registered with Apple. Click **Done** (not Move to Trash),
   then open **System Settings → Privacy & Security**, scroll down to
   *"Soundpad" was blocked to protect your Mac*, click **Open Anyway** and confirm with your
   password. After the first time, it opens normally.

## Get started in 2 minutes

1. **Add sounds.** Drag audio files or whole folders onto the Soundpad window, or click **＋**.
2. **Play a sound.** Double-click it, or select it and press **Return**.
3. **Give it a shortcut.** Right-click the sound → **Set Hotkey…**, then press a key
   combination such as **⌥1** or **⌃⇧A**. Shortcuts need at least one of ⌃ ⌥ ⇧ ⌘ so they
   don't get in the way of normal typing. Function keys (F1–F20) also work on their own.

## Play sounds in Discord, Zoom, Teams or games

1. In Soundpad, click **Install** in the orange banner at the top.
   macOS asks for your password once, and your sound pauses for about a second.
2. Optional: open **Settings** (⌘,) and set **Mix in my microphone** to your microphone,
   so people hear your voice as well. macOS asks for microphone permission the first time.
3. In your chat app or game, open its voice settings and choose **Soundpad Virtual Mic** as
   your **microphone / input device**.

> 💡 If sounds get cut off or sound quiet for others, turn off the chat app's
> **noise suppression** (in Discord this is called *Krisp*).

You hear sounds through your normal speakers or headphones. The two sliders in the player
bar set how loud the sounds are **for you** (🔊) and **for others** (🎙). Click an icon to mute
that side.

## Playlists

- Click **＋ New Playlist** at the bottom of the sidebar.
- Drop audio files from Finder onto a playlist, or drag sounds from **All Sounds** onto it.
  You can also right-click sounds → **Add to Playlist**.
- A sound can be in several playlists. Removing it from a playlist doesn't delete it from
  All Sounds.
- Drag sounds up and down to change their order.

## Playback modes

Click the mode button next to the time in the player bar:

| Mode | What happens when a sound ends |
|---|---|
| **Play Once** | Stops. Best for sound effects. |
| **Repeat Sound** | Starts the same sound again, with no gap. |
| **Play List** | Plays the next sound in the list, then starts over from the top. |

## Keyboard shortcuts

These work while the Soundpad window is active. Your own per-sound hotkeys work everywhere.

| Keys | Action |
|---|---|
| Return / double-click | Play the selected sound |
| Space | Pause / resume |
| Esc or ⌘. | Stop |
| ⌘[ / ⌘] | Jump back / forward 10 seconds |
| ⌥⌘[ / ⌥⌘] | Previous / next sound |
| ↑ / ↓ | Select a sound |
| ⌫ | Remove the selected sound (from the playlist, when you are in one) |
| ⌘O | Add sounds |
| ⇧⌘N | New playlist |
| ⌘, | Settings |

You can also set system-wide hotkeys for **Stop** and **Pause** in Settings.

## Updates

Soundpad checks for a new version once a day. When one is available, an **Update Available**
button appears in the toolbar. You can also check yourself in **Settings → Updates** or from
the **Soundpad** menu → **Check for Updates…**. Updates are digitally signed, so Soundpad
only installs genuine ones.

## Questions and problems

**My friends don't hear the sounds.**
Check that your chat app's microphone is set to **Soundpad Virtual Mic**, and that the 🎙
slider in Soundpad isn't at zero or muted.

**My friends hear the sounds but not my voice.**
In Soundpad's Settings, set **Mix in my microphone** to your real microphone, and allow
microphone access in System Settings → Privacy & Security → Microphone.

**I already use BlackHole (or Loopback).**
That's fine, they work side by side. Soundpad never takes over BlackHole by itself, so it
won't leak sounds into OBS or other apps that use it. If you'd rather send Soundpad into
BlackHole instead of installing Soundpad Virtual Mic, click **Use BlackHole 2ch instead** in
the banner, or choose it in Settings → **Virtual microphone**.

**Soundpad says "you may not hear your sounds".**
Your Mac's sound output is set to a virtual device such as BlackHole, so the sounds go there
instead of to your speakers. Click **Choose Speakers…** and pick your speakers or headphones.

**A hotkey doesn't work.**
Another app might already use that key combination. Pick a different one.

**"Soundpad" Not Opened: "Apple could not verify Soundpad is free of malware…"**
Click **Done**, then go to **System Settings → Privacy & Security** and click **Open Anyway**
next to *"Soundpad" was blocked*. You only need to do this once.
(Comfortable with Terminal? `xattr -dr com.apple.quarantine /Applications/Soundpad.app` does the same.)

**Where are my sounds stored?**
Soundpad doesn't copy your files. It remembers where they are on your Mac. If you move or
delete a file, Soundpad can't play it anymore. Your list and settings are saved in
`~/Library/Application Support/Soundpad/`.

**How do I uninstall it?**
To remove the virtual microphone, open Settings → Soundpad Virtual Mic → **Manage** →
**Uninstall**. Then drag Soundpad from Applications to the Trash.

## Privacy

Soundpad doesn't collect any data and has no accounts or ads. The only time it connects to the
internet is to check this page for updates, and you can turn that off in Settings → Updates.

## Help make Soundpad better

Found a bug or have an idea? [Open an issue](https://github.com/bynarig/soundpad/issues).
Want to help with code or translations? See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Soundpad is free and open-source software, released under the
[GNU General Public License v3.0](LICENSE). Created by bynarig and Claude.

The virtual microphone is built from the open-source
[BlackHole](https://github.com/ExistentialAudio/BlackHole) driver by Existential Audio
(GPL-3.0). It is renamed to *Soundpad Virtual Mic*, as BlackHole's license terms require.
