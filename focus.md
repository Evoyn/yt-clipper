We're putting `nextprompt.md` aside for now. Let's focus on making the application production-ready. The podcast caption quality is already good enough, so our priority is polishing the product, improving UI and UX, and making the workflow feel like a real desktop application instead of a prototype.

# Tasks

## 1. Modernize the Egui Interface

The current Egui UI feels outdated. Redesign the application with a modern, professional desktop application look.

Goals:

* Clean layout with proper spacing
* Better typography
* Modern buttons and controls
* Better color hierarchy
* Consistent styling across all pages
* Improved icons where appropriate
* Better visual feedback (hover, selected, disabled, loading)
* Overall make it feel polished and production-ready.

Feel free to reorganize the layout if it improves the user experience.

---

## 2. Redesign the Caption Editor

Instead of only displaying the current caption, I want a full transcript editor.

Example:

```text
00:00.00  Hello everyone, welcome to today's podcast.
00:04.25  Today we're talking about...
00:08.70  ...
```

Requirements:

* Show every caption.
* Every caption should be editable.
* Users should be able to:

  * edit text
  * add captions
  * delete captions
  * split captions
  * merge captions
* Timestamps should remain editable.
* Use `minutes:seconds:centiseconds` (or milliseconds if more appropriate).
* Make it easy to censor words.
* Editing should update the preview immediately.

If you have a better UX than my suggestion, feel free to implement it.

---

## 3. Podcast Speaker Detection

Since this application mainly targets podcasts "for now", we need much better framing.

Implement a Podcast Mode that:

* detects every visible face
* identifies who is currently speaking
* automatically follows the active speaker
* handles conversations naturally
* keeps smooth camera movement instead of constantly jumping between faces

This system will later drive automatic Shorts generation.

---

## 4. Build a Production-Ready Video Preview Editor

Instead of immediately rendering after clicking **Promote** or **Render**, users should enter a lightweight video editor.

The editor should feel similar to a simplified CapCut or Premiere designed specifically for Shorts.

### Preview Performance

Generate or download a low-quality preview first so playback remains smooth without waiting for the final render.

---

### Layout

I imagine something similar to:

```
---------------------------------------------------------------
Toolbar

Timeline              |        Video Preview
                      |
Clip Timeline         |     Horizontal Video
                      |     Draggable 9:16 Crop Box
                      |
---------------------------------------------------------------
Caption List          |     Properties
                      |
00:00 Hello...        | Face Tracking
00:03 Welcome...      | Caption Style
00:08 ...             | Crop Settings
                      | Export Settings
---------------------------------------------------------------
```

Feel free to improve this layout if you have a better idea.

---

### Timeline

Instead of only showing the preview, add a timeline where users can quickly scrub through the clip.

The timeline should visualize:

* clip duration
* captions
* speaker changes
* camera switches
* detected highlights

Users should be able to click anywhere to preview instantly.

---

### Interactive 9:16 Crop Tool

The preview should include a draggable 9:16 crop box.

Users should be able to:

* drag
* resize
* zoom
* reset
* use keyboard shortcuts for precise movement

This should feel similar to editing in CapCut.

---

### AI Camera Modes

Support multiple framing modes.

* Manual
* Center
* Auto Face Detection
* Active Speaker (recommended for podcasts)
* Group Mode

For podcasts, Active Speaker mode should automatically:

* detect who is talking
* smoothly move the camera
* zoom appropriately
* avoid excessive switching
* keep faces centered

The goal is to mimic a human editor.

---

### Face Detection Overlay

Display every detected face with a tracking overlay.

Example:

```
Person A
Person B
Person C
```

If Auto Mode is enabled:

```
Tracking Person B
Confidence: 96%
```

Users should be able to manually override the selected speaker by clicking another detected face.

---

### Speaker Timeline

Visualize who is speaking over time.

Example:

```
A ███████
B        ████████
A                 ███
C                    ██████
```

This makes it easy to verify that speaker detection is correct.

---

### Caption Preview

The preview should display caption styling in real time.

Users should be able to adjust:

* font
* size
* weight
* color
* outline
* shadow
* background
* position

There's no need to render every subtitle in preview. Showing representative examples is enough to preview styling changes.

---

### Caption Style Presets

Instead of requiring users to configure every option manually, include presets.

Examples:

* Classic
* TikTok
* Podcast
* Minimal
* Gaming
* MrBeast-style

Users can still customize after selecting a preset.

---

### Safe Area Overlay

Display the safe area so captions and important content don't get covered by YouTube Shorts or TikTok UI elements.

---

### Before / After Preview

Allow users to compare:

* Original video
* AI-edited version

This makes it easy to verify framing improvements.

---

### Export Summary

Before rendering, show a summary.

Example:

* Clip Length
* Resolution
* Caption Enabled
* Speaker Tracking Enabled
* Estimated Render Time

Then allow the user to go back or render.

---

## 5. Investigate Clip Duration

I've noticed almost every detected moment is around **30 seconds**.

Please investigate why.

Questions:

* Is there a hardcoded limit?
* Why aren't there more 40–60 second clips?
* The maximum should be configurable up to **3 minutes** (following YouTube Shorts standards).

The clipping algorithm should choose the most natural clip length instead of defaulting to ~30 seconds.

---

## 6. Improve Generated Titles

The generated titles don't feel production-ready.

Improve title generation so titles:

* work well as YouTube Shorts titles
* have stronger hooks
* increase click-through rate
* remain relevant to the content
* avoid generic wording
* avoid excessive clickbait

The goal is titles that creators can upload without rewriting.

---

# Workflow Goal

The ideal production workflow should be:

```
AI detects interesting moments
        ↓
User selects a clip
        ↓
Video Preview Editor
        ↓
AI Speaker Tracking
        ↓
Adjust 9:16 crop (optional)
        ↓
Edit captions (optional)
        ↓
Choose caption style
        ↓
Preview
        ↓
Render
```

The application should feel polished enough that most users can go from detection to a finished Short in under a minute, with AI handling most of the work while still allowing quick manual adjustments.

---

Once all of the above is implemented, stop and let me review everything before moving on to the next stage.
