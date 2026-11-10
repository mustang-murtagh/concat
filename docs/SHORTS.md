# Concat Shorts

Turn one long recording into a handful of finished short clips by asking Claude.

You say *"make shorts from this recording"*. Concat transcribes it on your machine, suggests the best moments, and waits for your yes. Then it renders each clip you approved in **vertical 9:16, landscape 16:9 and square 1:1**, with pauses cut, the speaker framed, word-by-word captions, your brand on it and the sound cleaned up.

Nothing is uploaded. The only network use is downloading a recording you gave a link to, and a one-off download of the speech model.

- [Set up](#set-up) (once, about 15 minutes)
- [Use it](#use-it)
- [Your brand kit and other settings](#settings)
- [What it does to a clip](#what-it-does-to-a-clip)
- [Tools](#tools)
- [When something goes wrong](#when-something-goes-wrong)

## Set up

### 1. Install what it needs

| Need | Why | macOS | Windows | Linux |
| --- | --- | --- | --- | --- |
| Rust 1.93+ | builds Concat | [rustup.rs](https://rustup.rs) | [rustup.rs](https://rustup.rs) | [rustup.rs](https://rustup.rs) |
| FFmpeg 7+ libraries, cmake, a C++ compiler | reading and writing video | `brew install ffmpeg cmake` | see [Build from source](../README.md#build-from-source) | see [Build from source](../README.md#build-from-source) |
| yt-dlp *(only for page links)* | fetches YouTube, Zoom, Vimeo, Loom links | `brew install yt-dlp` | `winget install yt-dlp` | `pipx install yt-dlp` |

Local files, direct video links, and Google Drive or Dropbox share links work without yt-dlp.

Rust picks up the exact compiler version from `src/rust-toolchain.toml`. Every Rust dependency is pinned in `src/Cargo.lock`; `cargo` fetches them on the first build.

### 2. Build

```sh
cd src
```

```sh
cargo build --release -p concat-mcp
```

That makes `src/target/release/concat-mcp` (`concat-mcp.exe` on Windows): the Concat engine with the shorts tools, and no window. The first build takes a while.

Want the editor window too, for finishing a clip by hand? Build it as well. `concat --mcp` serves the same tools.

```sh
cargo build --profile app -p concat
```

### 3. Connect it to Claude

Use the **full path** to the binary you built.

**Claude Desktop** - Settings > Developer > Edit Config, then restart Claude:

```json
{
  "mcpServers": {
    "concat": {
      "command": "/full/path/to/concat/src/target/release/concat-mcp",
      "args": ["headless"]
    }
  }
}
```

**Claude Code:**

```sh
claude mcp add concat -- /full/path/to/concat/src/target/release/concat-mcp headless
```

On Linux, if the FFmpeg you built against is not on the system library path, add `"env": {"LD_LIBRARY_PATH": "/path/to/ffmpeg/lib"}` to the server's entry.

### 4. Point it at your logo

The colours, fonts and end card are already set to the AI with Sally identity. The logo is the one thing to add. Tell Claude:

> Set my shorts logo to /Users/me/Brand/logo-horizontal.png

**Fonts:** captions use Montserrat and the end card's name uses EB Garamond. Install both on your computer (they are free on Google Fonts) and they are picked up. No install? Point the settings at the font files instead: *"set the Montserrat font file to /Users/me/Fonts/Montserrat-ExtraBold.ttf"*. A font that is found nowhere is drawn in Concat's own typeface, and the render says so.

## Use it

Talk to Claude. Three ways to choose the clips, and you can mix them:

| You want | Say |
| --- | --- |
| The tool to suggest, you approve | *"Make shorts from /Users/me/Movies/webinar.mp4"* |
| Claude to pick | *"Make shorts from this link: ... Read the transcript and pick the three strongest moments about pricing"* |
| Your own timestamps | *"Make shorts from webinar.mp4: 4:10 to 5:25 and 31:00 to 32:20"* |

What happens:

1. **Prepare.** The recording is fetched if it is a link, transcribed and scanned for pauses. Expect a few minutes for an hour of recording, more on an older machine; Claude reports progress while it waits.
2. **Plan.** You get a list: title, start, end, length, and why each moment was suggested. Nothing renders yet.
3. **Approve.** Say which to make. Move an edge (*"start c2 ten seconds earlier"*), add one, drop one, or ask to see one first (*"show me c3 in vertical"*).
4. **Render.** Each approved clip is exported in each shape.

Useful things to say along the way:

- *"It's a screen recording"* / *"it's me talking to camera"* / *"it's a Zoom meeting"* - settles how the picture is framed.
- *"Make them 30 to 45 seconds"* or *"give me 10 suggestions"*.
- *"Only render vertical."*
- *"Crop c2 further left."* / *"Show the whole screen on c4 instead of cropping."*
- *"What did I say about onboarding?"* - Claude reads the transcript.

### Where things land

Everything for a job is in one folder: `~/Movies/Concat Shorts/<job>/` on macOS, `~/Videos/Concat Shorts/<job>/` elsewhere.

| In it | What |
| --- | --- |
| `clips/` | the finished files: `02-your-title-c2-vertical.mp4` |
| `projects/` | a Concat project per clip per shape. Open one in the Concat window to change anything by hand and export again |
| `transcript.json` | every word with its time |
| `job.json` | the plan and how far each render got |

A job survives a restart. If preparing was cut short, ask Claude to *"carry on with job ..."*.

## Settings

One file, `shorts.json`, in Concat's config folder. Ask Claude *"show my shorts settings"* for where it is and what is in it, and change anything by saying so: *"make shorts 45 to 75 seconds"*, *"turn the end card off"*, *"put the logo bottom left"*. You can also edit the file; a field you leave out keeps its default, and a misspelt field is refused by name.

### Brand kit - `brand`

| Setting | Default | What it is |
| --- | --- | --- |
| `name` | `AI with Sally` | the end card's headline when there is no logo |
| `colors` | turquoise `#2BD4D1`, linen `#FDF9F0`, porcelain `#F6FBFB`, ink `#1E1B18`, slate `#33475B`, clay `#C96A4B`, coral `#F2593F`, chartreuse `#C6E82E` | the palette. Every other colour setting takes one of these names, or a `#rrggbb` |
| `fonts` | Montserrat, EB Garamond | each `{family, file}`; `file` only when the font is not installed |
| `logo.file` | *(none)* | a PNG, transparent background for preference |
| `logo.position` | `topRight` | `topLeft`, `topRight`, `bottomLeft`, `bottomRight` |
| `logo.width` | `0.3` | fraction of the frame's width |
| `logo.margin` | `0.05` | gap to the edges, fraction of the frame's width |
| `logo.opacity` | `1` | 0 to 1 |
| `endCard.enabled` | `true` | each clip ends on the card |
| `endCard.seconds` | `2.5` | how long it holds |
| `endCard.background` / `text` | `turquoise` / `ink` | the surface and what is written on it |
| `endCard.headline` | *(brand name)* | the large line; the logo stands there instead when one is set |
| `endCard.headlineFont` / `headlineWeight` | `EB Garamond` / `500` | |
| `endCard.descriptor` | `AI ADVISORY AND COACHING` | small capitals under the rule |
| `endCard.url` | `AIWITHSALLY.COM` | the last line |
| `endCard.font` | `Montserrat` | the descriptor's and the address's typeface |

The defaults follow the AI with Sally design system: turquoise as the lead surface with ink text on it, text only ever ink or porcelain, the end card set at the lower left rather than centred, no rounded corners.

### Captions - `captions`

| Setting | Default | What it is |
| --- | --- | --- |
| `enabled` | `true` | |
| `mode` | `highlight` | `highlight`: a phrase at a time, each word coming to full strength as it is said. `phrase`: a phrase at once. `word`: one word at a time |
| `font` / `weight` | `Montserrat` / `800` | |
| `text` | `ink` | the words' colour |
| `plate` | `turquoise` | the block behind the words; empty for none, and then the words wear an outline |
| `outline` | `porcelain` | the outline's colour when there is no plate |
| `dim` | `0.45` | how strongly the words still to come show, 0 to 1 |
| `punctuation` | `false` | keep commas and full stops (question and exclamation marks always stay) |
| `maxWords` | `5` | most words in one phrase |
| `uppercase` | `false` | |

### Clips, pauses and sound

| Setting | Default | What it is |
| --- | --- | --- |
| `clips.minSeconds` / `maxSeconds` | `60` / `90` | length of a suggested clip, after pauses are cut |
| `clips.maxClips` | `6` | most suggestions per recording |
| `tighten.enabled` | `true` | cut silences and dead air |
| `tighten.minPause` | `0.7` | a pause this long, in seconds, is cut |
| `tighten.keep` | `0.18` | seconds of it left either side: the breath |
| `audio.enabled` | `true` | clean the sound |
| `audio.denoise` | `30` | Concat's Noise Reduction, 0 to 100; 0 leaves it out |
| `audio.enhanceVoice` | `50` | Concat's Enhance Voice, 0 to 100; 0 leaves it out |
| `audio.loudness` | `-14` | level every clip to this many LUFS (-30 to -12); 0 leaves the level alone |

### Frame shapes, reframing and export

| Setting | Default | What it is |
| --- | --- | --- |
| `formats` | `vertical` 1080x1920, `landscape` 1920x1080, `square` 1080x1080 | each `{id, label, width, height, enabled, captionY, captionSize, captionChars, safeInset}`. Set `enabled` to `false` to stop making a shape |
| `reframe.mode` | `auto` | `auto`: decide by what is in the picture. `crop`: always fill. `fit`: always show the whole picture |
| `reframe.blur` | `0.6` | softness of the blurred copy behind a whole picture, 0 to 1 |
| `export.codec` | `h264` | `h264`, `hevc` or `av1` |
| `export.crf` | `20` | quality: lower is better and bigger |
| `export.preset` | `medium` | encoder speed, `ultrafast` to `veryslow` |
| `export.maxFps` | `30` | a 60 fps recording is rendered at 30 |
| `transcription.model` | `base.en` | `tiny.en` (fastest), `base.en`, `small.en` (best wording, slowest). Downloaded once, 75 to 490 MB |
| `download.ytDlp` | `yt-dlp` | the program's name or full path |
| `download.maxHeight` | `1080` | tallest picture fetched from a link |
| `workspace` | *(videos folder)* | where job folders are kept |

Changed a setting after rendering? Say *"render them again"*.

## What it does to a clip

**Picks the moment.** Every run of whole sentences that comes to the wanted length is scored on how it opens (a question, a line that promises something, "you", a number - and against it, an opening that leans on what came before), how it closes (a finished sentence, before a pause, not where the next sentence carries the thought on), whether it starts after a pause, how much it says that the rest of the recording does not, and its pace. The best that do not overlap are suggested, each with its reason. It counts; it does not understand. That is why the plan is yours to approve and Claude can pick instead.

**Cuts on words.** Times you or Claude give are moved onto the nearest word boundaries, so a clip never starts or ends mid-word.

**Tightens.** The recording's own loudness decides what is quiet. Pauses of 0.7 seconds or more are cut down to a breath, and silence at the clip's two ends is trimmed. A recording with music under it throughout has no pauses and is cut nowhere.

**Frames.** A few frames of each clip are looked at with Concat's built-in person model:

| In the picture | In a frame of another shape |
| --- | --- |
| one person, filling a fair part of it | cropped in, centred on where they are |
| nobody, or a small face in a corner bubble (a screen recording) | shown whole over a blurred copy, so text stays readable |
| people across the width (a meeting grid, a panel) | shown whole over a blurred copy |

Say what the recording is, or how one clip should be framed, and that is used instead.

**Captions.** The transcript's own word times, in your brand's type.

**Brands.** Logo in the corner throughout; the end card after the last word.

**Cleans the sound.** Noise reduction, voice enhancement and loudness levelling across the whole clip, using Concat's own audio effects.

Each of these is an ordinary clip or effect in the project, which is why a render can be opened in the Concat window and changed.

## Tools

What Claude calls. You never need to; this is for reference.

| Tool | Does |
| --- | --- |
| `shorts_start` | begins a job on a file path or a link; returns at once |
| `shorts_wait` | waits for preparing, with progress |
| `shorts_status` | one job, or every job |
| `shorts_transcript` | what was said, with times |
| `shorts_clips` | adds or changes clips by their times, framing and title |
| `shorts_suggest` | suggests again with a different count or length |
| `shorts_review` | approves, rejects, removes |
| `shorts_preview` | one frame of a clip as it will render |
| `shorts_render` | renders the approved clips; call again until none are left |
| `shorts_cancel` | stops a job |
| `shorts_settings` / `shorts_settings_update` | reads and changes the settings |

They sit beside Concat's editing tools ([MCP.md](MCP.md)), and build every clip with the same edit commands those use.

## When something goes wrong

| It says | Do |
| --- | --- |
| *this link is a page, not a video file, and fetching it needs yt-dlp* | install yt-dlp (table above), or download the recording yourself and give its path |
| *yt-dlp could not fetch the link* | the recording is private or needs a sign-in. Download it in your browser and give the file's path |
| *the speech model could not be downloaded* | check the connection and ask again; it is a one-off download |
| *the linked FFmpeg has no encoder "H.264"* | Linux only: the FFmpeg you built against lacks OpenH264. Build against one that has it: the BtbN `shared` builds the README names do |
| *preparing was interrupted* | ask Claude to carry on with that job; the download and transcript are kept |
| a caption word is wrong | open the clip's project in the Concat window and fix the title, or use `small.en` for better wording |
| a crop misses the speaker | *"crop c2 further left/right"*, or *"show the whole picture on c2"* |
| the tool is refused a folder | it writes only under your home folder. Set `CONCAT_MCP_ROOTS` in the server's `env` to allow others |

Server messages go to Claude's MCP log (Claude Desktop: Settings > Developer > the server's log).

## Licence

Concat is AGPL-3.0-or-later, and so is this. Using it yourself carries no obligation. Giving a build to somebody else means giving them the source too.
