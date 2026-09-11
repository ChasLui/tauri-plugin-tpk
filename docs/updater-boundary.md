---
title: The updater boundary
description: What ships as a pack, what ships as a binary, and how to run both.
---

# The updater boundary

Two update mechanisms, two schedules, one rule:

> **TPK updates what the frontend *is*. The store updates what the app *does*.**

`tauri-plugin-tpk` never replaces a binary. `tauri-plugin-updater` and the app
stores never touch content packs. They do not talk to each other, and they
should not.

## Which side is a change on

| Change | Route |
|---|---|
| HTML, CSS, JS, images, copy, layout | TPK |
| A bug in existing frontend logic | TPK |
| A new screen inside an existing feature | TPK |
| Copy for an A/B test | TPK |
| A new Tauri command | Store |
| A new capability or permission | Store |
| A new top-level feature area | Store |
| A new purchase flow | Store (and IAP) |
| New data collection | Store (and a privacy label change) |
| A dependency on a newer platform API | Store |
| A Rust dependency bump | Store |

The frontier is not "how big is the diff", it is "does the native surface
change". A 2 MB frontend rewrite that calls exactly the same commands is a pack.
A three-line Rust change is a binary.

## `min_shell` is the contract

When a pack needs a command the shell may not have, say so:

```bash
tpk pack --kind patch --min-shell 1.5.0 ...
```

The client skips packs the running shell is too old for. If nothing is left,
`check()` returns `{ status: "shell_required", min_shell }` — route the user to
a store update, not to a retry.

This is the entire coupling between the two mechanisms, and it points one way:
content declares what it needs, and the shell never declares what content it
wants.

## Running both

```rust
fn main() {
    let mut context = tauri::generate_context!();
    let tpk = tauri_plugin_tpk::attach(&mut context);

    tauri::Builder::default()
        .plugin(tauri_plugin_tpk::init(tpk))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .run(context)
        .expect("error running app");
}
```

They are independent. The shell updater replaces the binary, which brings new
embedded assets; the content layers stack on top of whatever the new binary
embeds. Nothing needs to be reset — the overlay index is rebuilt from
`state.json` and the layer pool on the next launch either way.

One thing to get right: after a shell update the embedded floor has moved, but a
`base` pack still shadows it completely. If a shell release changes content the
frontend depends on, publish a matching pack in the same window.

## A combined update prompt

```ts
const [content, shell] = await Promise.all([check(), updaterCheck()]);

if (shell) {
  // Native update wins: it may carry the commands the content needs.
  await promptShellUpdate(shell);
} else if (content.status === "shell_required") {
  await promptShellUpdate();  // content is waiting on a binary
} else if (content.status === "available") {
  await download();
  toast("Update ready — it will apply next time you open the app.");
}
```

Prefer the shell update when both are available. Installing content that needs a
newer shell and then being told to restart twice is a worse experience than
updating once.

## Do not reload the WebView

There is no supported way to apply a pack to a running process. A `location
.reload()` after `download()` gives you a WebView that has already imported
modules from the old revision and now fetches the rest from the new one, and the
resulting stack trace is not reproducible.

`download()` returning `staged` means "next cold start". Say that to the user.

## Rollback asymmetry

A bad pack rolls back automatically after three unacknowledged launches. A bad
binary does not — that is what store phased releases and your own staged rollout
are for.

This asymmetry is the reason to prefer the pack side for anything that can go
either way: the failure mode is bounded and self-correcting.
