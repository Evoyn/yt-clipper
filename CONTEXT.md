# YT Clipper

A Windows-first desktop app that turns long VODs — gaming streams and podcasts — into vertical short-form clips: transcription, moment detection, framing, speaker tracking, captioning, and export all happen on the user's own machine.

## Language

**Creator**:
A streamer whose VODs the operator clips with their permission. A Creator carries saved defaults (facecam layout, caption style, spoken language) that apply to every VOD of theirs.
_Avoid_: channel, streamer, user

**VOD**:
The long source recording a project is built around — a finished stream recording on YouTube, or a local video file. Always belongs to a Creator.
_Avoid_: stream, video, source

**Moment**:
A scored candidate time range within a VOD, surfaced by analysis (or marked manually by the operator), awaiting review.
_Avoid_: highlight, candidate, event

**Signal**:
One strand of evidence behind a Moment's score — chat-rate, loudness, lexicon, arousal, or LLM judgment — kept separate (never blended away) so the ranking can be retuned and the operator can see *why* a Moment surfaced.
_Avoid_: feature, metric, factor

**Arousal**:
The Signal measuring the emotional *activation* of the streamer's voice — how worked-up they sound — from a speech-emotion model's arousal axis. Deliberately ignores *which* emotion: a laugh, a rage, and a hype-moment all score high (all clip-worthy). Its job is to tell an emotional reaction apart from merely loud audio (a game explosion, music, a cutscene) that loudness alone cannot.
_Avoid_: sentiment / valence (positive-vs-negative — explicitly not measured), excitement (reserved for the lexicon), emotion (too broad — implies classifying which emotion)

**LLM judgment**:
The Signal from a local language model reading a Moment's transcript: a judgment of how clip-worthy the *streamer's* speech is, scored per-candidate and kept relative to the others. Unlike the lexicon (which counts excitement words), it weighs meaning in context — and it is explicitly told to discount scripted in-game dialogue and cutscene narration, so a dramatic game line is not clip-worthy unless the streamer reacts to it. Runs locally on the GPU after transcription unloads; no cloud.
_Avoid_: rerank (it is a Signal in the ensemble, not a post-sort stage), GPT / cloud (it is a local GGUF), sentiment (it judges clip-worthiness, not positive-vs-negative)

**Clip**:
A Moment the operator has promoted for production. A Clip gets framing, captions, and an export; a Moment that is never promoted gets nothing.
_Avoid_: short, video, segment

**Title**:
A catchy ≤60-character short-form title generated for a Moment by the LLM judge at detect time (it already reads the transcript — ADR 0010/0015), stored on the Moment and used to name the rendered Short (`<title>.mp4`) so a re-promote never overwrites a previous one. A manually-marked Moment has no generated title; the render then names the Short by timestamp (`clip-<m-ss>`). Distinct from the VOD's own `title` (the stream's name, which names the **stream folder**).
_Avoid_: caption (that is the on-screen burned text), filename, label

**Stream folder**:
A VOD's output directory, `workspace/<creator>/<stream-title>/` (sanitized from VOD metadata — ADR 0015). The rendered Shorts (named by their generated Title) sit at its root; every intermediate (analysis audio, downloaded Segments, `project.json`, captions, font) lives under its `data/` subfolder. It realizes Creator scoping on disk; the **Creator store** (`workspace/creators.json`) holds the matching per-Creator defaults.
_Avoid_: workspace (that is the parent holding every stream folder), project folder

**Creator store**:
The global `workspace/creators.json` (`core::CreatorStore`) of per-Creator remembered defaults, keyed by Creator name (ADR 0016) — the long-deferred `creators.json`. Today it remembers a Creator's **Caption Style**, **language**, and **Caption engine**: importing a known Creator's VOD seeds the render's genre/engine to their last-used ones, and an **Auto**-language import (the GUI/CLI default since 2026-07-02) applies the Creator's saved language — each render saves them back (operator overrides always win; explicit `en|id|ja` beats Auto; `YC_QWEN_ENS` overrides the engine per-invocation without being remembered). Seam/crop defaults are reserved for a later Creator-aware-framing slice.
_Avoid_: project.json (that is per-VOD, under the stream folder's `data/`), dialect store (that is per-language transcription corrections)

**Segment**:
The padded span of full-quality VOD video downloaded for a Clip when it is promoted — bounded by keyframes, so always a little longer than the Clip's range. The frame-accurate export is cut from the Segment; the VOD's video is never downloaded whole (ADR 0001, ADR 0006).
_Avoid_: clip, chunk, section, source

**Vocal stem**:
The streamer's voice track separated from a Segment's audio — music and game SFX stripped out by a source-separation model. It isolates *voice from music/SFX*, **not one speaker from another**: an in-game NPC or cutscene voice survives it (telling those apart is a separate, future concern). Produced per-Clip behind the `sep` feature. **Rejected for captions (ADR 0014)**: htdemucs is a music separator and drops real spoken speech, so the export captions the mixed audio; the validated runtime is kept off-by-default for a possible future arousal-discovery use.
_Avoid_: mic track / mic (it is not literally the streamer's microphone feed — any voice in the mix survives), denoised audio (it removes music/SFX, not just background noise)

**Cleaned voice**:
The caption-input audio after a gentle speech **denoiser** (DeepFilterNet, the `enh` feature) suppresses game SFX / music while *keeping* the streamer's voice — fed to whisper so it reads the voice above the noise on a masked Clip. Distinct from the **Vocal stem**: that *separates* voice from music and can drop quiet speech (ADR 0014); this *denoises* with an attenuation limit, so it never gates speech down to the dead silence whisper hallucinates into (ADR 0029). Caption-input only — the rendered Clip's audible audio is always the mix.
_Avoid_: vocal stem (that is separation), noise gate (it is a learned denoiser, not a threshold gate), enhanced audio (the Clip's audio is unchanged — only whisper's transcription input is)

**Dialect store**:
A curatable, **layered** set of per-language files (the `DialectLexicon`) that lifts the caption-transcription ceiling for a Creator's accent / slang / viewer names — the *linguistic* error that no audio processing fixes (ADR 0014). **Three layers merge per render, most-specific winning** (ADR 0031): the bundled **base** (`assets/dialect/<lang>.json` — the shared real-word dictionary + generic fixes + config), a **per-Creator** store (`workspace/<creator>/<lang>.json` — a streamer's confirmed slang/names, applied across all their VODs), and a **per-clip** store (`<stream>/<ClipStem>.<lang>.json`, beside the exported Short — *that clip's* auto-harvest, for easy per-export curation). It drives a deterministic `wrong → right` correction dict (always on); the **LLM correction pass** for context-sensitive overrides a global dict can't do safely (ADR 0030); opt-in whisper priming (`prime`, off — it drifts); and **auto-harvest**: words whisper was unsure of that aren't in the bundled real-word dictionary are appended as `unverified` to-dos to the **per-clip** store each caption run, so the operator's review queue self-populates per export. Each harvested entry's note records **where the word came from** — the generated Short Title + the absolute VOD timestamp (ADR 0022). A confirmed (filled `right`) per-clip entry **auto-promotes** to the per-Creator store on the next render, so curating once in the small per-clip file makes it durable for the Creator (ADR 0031) — unless it is marked **`clip_only`**, a fix scoped to a single clip (a real word meant as slang in just that clip) that must not globalize. The harvest dictionary always loads a bundled common-given-names list (`names.words.txt`, Indonesian + international — ADR 0023) so a correctly-read **common** name isn't flagged; an **unusual** viewer handle still harvests. Distinct from the per-Creator `names.json` roster (a streamer's actual viewer handles + garble→name corrections).
_Avoid_: lexicon (overloaded — that is the excitement-word list for detection scoring), dictionary (that is the bundled real-word wordlist + common-names list the harvest filters against, not the corrections)

**Review queue**:
A Creator's harvested caption to-dos awaiting curation — the `unverified` entries (blank `right`) in their Dialect store, each a word whisper misheard that the operator has not yet corrected. Surfaced in the app's detail pane (ADR 0032), grouped by the clip they were harvested from, so the operator fills the correct word in-app — hearing it in context via a VOD jump link (ADR 0022) — instead of hand-editing JSON. Filling an entry's `right` and saving removes it from the queue (it becomes a confirmed correction that applies to all the Creator's future clips). Reads and writes the **per-Creator** layer directly, the same layer auto-promote targets.
_Avoid_: to-do list / backlog (too generic), harvest (that is the act of populating the queue, not the queue itself)

**Layout**:
The arrangement of a Clip's 1080×1920 canvas. Two variants: stacked (gameplay Panel above facecam Panel, divided by the Seam) and full-frame (a single Panel filling the canvas — the gameplay, or the Facecam alone during a talking-session moment where the streamer's cam is the content). Chosen per Clip by M6 auto-detect (ADR 0011) unless the operator's **Layout preference** forces a specific one.
_Avoid_: composite, template, frame

**Layout preference**:
The operator's explicit choice of which Layout to frame a Clip with (ADR 0017), overriding M6 auto-detect: _auto_ (the ADR 0011 three-way decision), or a forced _stacked_ / _full cam_ / _full gameplay_. A forced choice still uses the detected Facecam Crop when one is found, else a sensible seed. A global session/per-invocation setting (GUI top-bar menu + a `--batch`/`--headless` CLI token), defaulting to _auto_; persisting it per-Creator is a later Creator-aware-framing slice (ADR 0016). It exists because the operator's preferred stacked framing must be selectable in `--batch`, which never opens the nudge editor.
_Avoid_: layout (that is the realized arrangement; this is the operator's pick of it), template

**Facecam**:
The region of the source video showing the streamer's webcam overlay. Its location and size are not fixed across a VOD — a small corner inset during gameplay, but the streamer may switch to a full-screen cam during a talking session — so it is detected per Clip from the promoted Segment's frames, not assumed (M6, ADR 0011). A cam can hold **more than one face** (a 2-person co-stream): all persistent faces are detected and the stacked facecam Panel frames their **union**, so neither person is cropped out (ADR 0026). The detected Facecam seeds the facecam Panel's Crop in a stacked Layout, or the whole canvas in a full-frame talking-session Layout.
_Avoid_: webcam (the hardware), facecam Panel (that is the canvas region it is shown in, not the source region)

**Panel**:
A region of the canvas that displays exactly one Crop of the VOD, filled edge-to-edge — never letterboxed, never stretched.
_Avoid_: slot, zone, view

**Seam**:
The horizontal boundary between the gameplay and facecam Panels in a stacked Layout. Its position is per-Clip, defaulted from the Creator.
_Avoid_: split, divider

**Crop**:
The rectangle of source-video pixels a Panel displays. Aspect-locked to its Panel: resizing zooms, dragging pans.
_Avoid_: selection, region, window

**Caption Style**:
A named preset describing how captions look and animate (font, colors, outline, animation genre — rolling-pop, huge-word, karaoke). Saved per Creator, overridable per Clip. The karaoke genre (enum `KaraokeFill`) highlights **per word as a whole, snapping** to the accent colour at each word's spoken onset (ASS `\k`, cumulative — ADR 0018), not a left-to-right fill.
_Avoid_: theme, template, skin

**Caption placement**:
The per-Clip override of where the caption block sits on the canvas and how large its text draws — set by dragging/resizing the captions over the editor's preview playback (ADR 0036). Absent (the default, and always in headless), captions keep the built-in anchor and the Caption Style's size. Presentation data on the Clip — never curation: a placement says nothing about words.
_Avoid_: caption position (underspecifies — placement also scales), caption style (that is the shared look/animation preset; placement is one Clip's geometry)

**Caption engine**:
The per-Creator choice of how a Clip's caption words are transcribed: **Whisper** (the single-decode default) or the **Qwen ensemble** (a five-variant vote with whisper as a voter, timing fused onto skeleton anchors and speech onsets — ADR 0034). A closed enum, not a model picker — the ensemble is a measured recipe, and a new model earns entry only through ADR 0034's gate. Remembered in the Creator store like Caption Style (ADR 0016); switching it for a curated Creator is a deliberate act (ADR 0033): the app states which of their existing corrections carry across (ADR 0035).
_Avoid_: model picker (a recipe, not a GGUF choice), decoder (ambiguous with whisper's internal decode config), ASR toggle

**Studio**:
The full-window editor page a Promote opens into (ADR 0039): transcript editor left, Before/After video preview center (Original = source frame + crop tools + face overlays; Preview = the composited 9:16 output + captions + safe-area guide), properties right (Camera / Framing / Caption style), timeline bottom (captions, speaker lanes, cut markers, playhead). Prepare auto-queues the caption pre-pass (`Job::Transcribe`) so the transcript is editable before any render; Export shows a summary (length / resolution / captions / camera / estimated time) before rendering.
_Avoid_: nudge editor (the old docked panel it replaced), preview window (it is a page, not a popup)

**Camera mode**:
How the Studio frames a Clip: **Manual** (the operator's own crops), **Center**, **Auto face** (a static crop on the most persistent face), **Active Speaker** (the cut-based plan following whoever talks — the podcast recommendation), or **Group** (everyone at once; 2 people = the stacked split screen). The AI modes derive framing from the Speaker analysis; dragging the frame in an AI mode flips to Manual.
_Avoid_: layout preference (that is the pre-editor stacked/full choice, ADR 0017), tracking mode

**Speaker track**:
One tracked person in a podcast Clip (`yc_frame::speaker`): a persistent face position (podcast cameras are static), per-bin **mouth activity** (luma change over a fixed grid in the lower face box), and the "Person A/B/C" label (left-to-right). The **Speaker analysis** (ADR 0038) gates activity by an audio VAD and attributes a speaker per time bin with a switch margin + confirmation hold, giving the editor's face overlays, speaker timeline, and confidence chip.
_Avoid_: diarization (that is the audio-embedding approach this deliberately isn't — yet), face cluster (that is M6's Facecam detection; a track adds time-series activity)

**Camera plan**:
The cut-based dynamic framing of an Active-Speaker Clip: contiguous **Shots**, each a clip-relative time span framed by one static Layout (solo 9:16 crop, or a split screen for a group shot). Cuts, not pans — human podcast editors cut; a virtual camera panning a static wide shot reads as amateur. Minimum shot length, flicker absorption, and rapid exchanges collapsing into a group shot keep it calm (ADR 0038). Rendered as a per-shot trim/crop concat in one ffmpeg pass (`camera.fg`), captions burned once over the joined stream. The operator overrides a shot by clicking another face at that time.
_Avoid_: keyframes (nothing interpolates), camera path (implies motion inside a shot)

**Caption preset**:
A named, complete `CaptionStyle` bundle the Studio's Caption panel starts from — Classic, TikTok, Podcast, Minimal, Gaming, MrBeast — pure data over the extended style fields (outline width/colour, shadow, back box, bold; ADR 0004/0039). Picking one replaces the whole style; every field stays editable after, and any tweak deselects the chip. The Creator store still remembers only the *genre* (ADR 0016).
_Avoid_: theme, template (both suggest something beyond field values)

**Transcript override**:
The operator's edited transcript from the Studio's caption panel (edit / add / delete / split / merge / censor, `m:ss.cc` timestamps), shipped with a Render and burned **verbatim** — no whisper, no harvest, no silence-drop, no re-timing (ADR 0039): the automated timing machinery exists to clean whisper's guesses, not the operator's words. Per-clip and immediate, unlike Dialect-store curation (durable per Creator, ADR 0031).
_Avoid_: correction (that is the dialect/LLM pass over whisper output), custom captions

**Offline**:
The core constraint: all analysis and rendering happens on the local machine. The only permitted network use is user-initiated ingestion of a VOD.
_Avoid_: air-gapped, local-only (both overstate it — ingestion may use the network)
