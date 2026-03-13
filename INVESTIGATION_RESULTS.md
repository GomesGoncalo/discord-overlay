# Discord Overlay Invisibility Investigation - FINAL REPORT

## Executive Summary

**Why the overlay is invisible:**

The Wayland compositor skips compositing the overlay surface because **damage region declarations are missing** from all rendering commits. The overlay is rendered correctly internally, but Wayland never composites the frames because no damage region is declared.

---

## Investigation Findings

### 1. idle_alpha Opacity Initialization ✅

**Status:** Working correctly

**Location:** `main.rs:298`
```rust
idle_alpha: 0.0,
```

**How it works:**
- Initialized to 0.0 (fully transparent)
- Correctly applied: `op = self.opacity * self.idle_alpha` (state.rs:507)
- All color channels multiplied by this opacity
- Animates from 0→1 over ~250ms when `in_channel = true` (main.rs:183-194)
- Speed: 0.064 per 16ms frame

**Verdict:** NOT the problem. Opacity system works correctly.

---

### 2. Damage Region Tracking ❌ **ROOT CAUSE**

**Status:** CRITICAL - Missing entirely

**Search result:** Zero occurrences of `damage()` or `wl_surface_damage()` in entire codebase

**Missing at these locations:**
- `state.rs:274-275` - Compact mode: after `egl.swap()`, before `commit()`
- `state.rs:889-890` - Normal mode: after `egl.swap()`, before `commit()`
- `state.rs:283` - Input region: after `set_input_region()`, before `commit()`

**Why this is critical:**

Wayland requires surfaces to declare which regions have changed via `damage()` calls:

```
CORRECT PATH (what should happen):
  ✓ egl.swap_buffers() — buffer updated
  ✓ wl_surface.damage(0, 0, 360, 64) — declare change region
  ✓ wl_surface.commit() — submit
  ✓ Wayland compositor composites surface
  ✓ Result: VISIBLE

CURRENT PATH (actual behavior):
  ✓ egl.swap_buffers() — buffer updated  
  ✗ wl_surface.damage() — MISSING!
  ✓ wl_surface.commit() — submit
  ✗ Wayland compositor: "Buffer changed, no damage region → skip"
  ✗ Result: INVISIBLE
```

**Verdict:** This is THE ROOT CAUSE of invisibility.

---

### 3. Layer Surface Configuration ✅

**Status:** Correct

- Anchor: `TOP | LEFT` (main.rs:62)
- Margin: 20px offset (main.rs:66)
- Exclusive zone: `-1` (overlay, non-exclusive) (main.rs:65)
- Layer: `Layer::Overlay` (main.rs:60)
- Visibility: Visible by default

**Verdict:** NOT the problem. Configuration is correct.

---

### 4. Surface State at Startup ⚠️

**Status:** No critical issues, minor improvement possible

- Surface created and configured ✓
- No buffer attached initially ⚠️ (minor)
- First buffer attached when Discord connects
- idle_alpha = 0.0 at that point (transparent initially)

**Verdict:** Not blocking. Startup happens async; first render called when Discord connects.

---

### 5. Initial Render Before Discord ❌

**Status:** No initial render, but secondary issue

**Where first draw() is called:**
- `main.rs:94` - Discord event handler (when Discord connects)
- `main.rs:224` - Animation timer (subsequent frames)

**Verdict:** Not blocking because rendering starts soon (when Discord connects) and fixes would apply either way.

---

## Complete Execution Trace

### Phase 1: Startup

```
main()
  ├─ Create wl_surface
  ├─ Create layer_surface (Layer::Overlay)
  ├─ Configure: anchor, size, margin, exclusive_zone
  ├─ layer.commit()  ← NO BUFFER YET
  ├─ EGL context initialized
  ├─ App created with idle_alpha=0.0, in_channel=false
  └─ event_loop.run()  ← waiting for events
```

### Phase 2: Discord Connects

```
Discord ready event
  └─ handlers.rs: handle_discord_event()
     ├─ in_channel = true
     ├─ idle_alpha = 0.0 (NOT YET ANIMATED)
     └─ return true → app.draw()

First app.draw() [state.rs:502]
  ├─ let op = 0.8 * 0.0 = 0.0 (fully transparent)
  ├─ Render all elements at 0% opacity
  ├─ egl.swap()  ← buffer ready
  ├─ wl_surface.commit()  ← ✗ NO damage() call!
  └─ Wayland compositor: "Skip this surface"
```

### Phase 3: Opacity Animation (250ms)

```
Timer fires every 16ms for ~15 frames
  ├─ Frame 1: idle_alpha=0.064 (6.4%)   → draw() → commit() [NO damage!] → skip
  ├─ Frame 2: idle_alpha=0.128 (12.8%)  → draw() → commit() [NO damage!] → skip
  ├─ Frame 3: idle_alpha=0.192 (19.2%)  → draw() → commit() [NO damage!] → skip
  ├─ ...
  └─ Frame N: idle_alpha=1.000 (100%)   → draw() → commit() [NO damage!] → skip

Result: All frames rendered but NONE composited
        Overlay INVISIBLE throughout fade-in
```

---

## The Fix (3 Locations)

### Location 1: state.rs, lines 274-275 (compact mode)

```rust
// BEFORE:
self.egl.swap();
self.layer.wl_surface().commit();

// AFTER:
self.egl.swap();
self.layer.wl_surface().damage(0, 0, self.width as i32, self.height as i32);
self.layer.wl_surface().commit();
```

### Location 2: state.rs, lines 889-890 (normal mode)

```rust
// BEFORE:
self.egl.swap();
self.layer.wl_surface().commit();

// AFTER:
self.egl.swap();
self.layer.wl_surface().damage(0, 0, self.width as i32, self.height as i32);
self.layer.wl_surface().commit();
```

### Location 3: state.rs, line 283 (clear input region)

```rust
// BEFORE:
self.layer.set_input_region(Some(region.wl_region()));
self.layer.wl_surface().commit();

// AFTER:
self.layer.set_input_region(Some(region.wl_region()));
self.layer.wl_surface().damage(0, 0, self.width as i32, self.height as i32);
self.layer.wl_surface().commit();
```

### Optional: main.rs, after line 313

```rust
// Add initial render before event loop (good practice)
app.draw();  // Render initial transparent frame before event_loop.run()
```

---

## Expected Result After Fixes

✓ When Discord connects:
  - Wayland compositor receives damage declaration
  - Compositor composites the surface
  - Overlay appears (at 0% opacity, still invisible but composited)

✓ Every 16ms for 250ms:
  - idle_alpha animates 0.0 → 1.0
  - Each frame declares damage and gets composited
  - Overlay fades in smoothly

✓ Final state:
  - idle_alpha = 1.0
  - Overlay fully opaque and VISIBLE
  - Proper fade-in behavior

---

## Summary

| Question | Answer | Status |
|----------|--------|--------|
| idle_alpha initialization? | Correctly initialized at 0.0, animates properly | ✅ Works |
| Damage region tracking? | **ZERO damage() calls found** | ❌ **ROOT CAUSE** |
| Layer surface config? | Anchor, margin, zone all correct | ✅ Works |
| Surface state at startup? | Configured but no initial buffer | ⚠️ Minor |
| Initial render before Discord? | Not called before event loop | ⚠️ Minor |

**Primary Issue:** Missing Wayland damage declarations
**Secondary Issues:** None (opacity and config are correct)

---

