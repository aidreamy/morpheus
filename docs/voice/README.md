# Voice (text-to-speech)

Morpheus can speak the agent's final answer with a local Kokoro text-to-speech
model. Everything runs on the machine: the model is loaded once by a resident
daemon (`morpheus-ttsd`), the audio is written next to the session, and the TUI
plays it.

This page is the complete reference for the voice feature: what the speakers
are, what their identifiers mean, how the language frontend is selected, and
how to configure everything through `setup.json` without the TUI.

## Quick start

1. Build with the voice feature (see [Build features](#build-features)).
2. Download a Kokoro bundle into `<cache>/morpheus/models/`.
3. Pick a voice in `/settings` → **Voice** → **Speaker**.

The TUI writes `voice.speaker` and `voice.language` together, so you never have
to know a numeric id or an espeak-ng code. Manual users can do the same in
`setup.json`; a speaker is enough, because the language is derived from it.

## Build features

The voice engine is optional because it links ONNX Runtime.

| Feature | What it adds | Required for |
| --- | --- | --- |
| `speak` | The Kokoro engine, the daemon, the TUI controls | English and Chinese (lexicon frontend) |
| `speak-phonemize` | `piper-phonemize` (espeak-ng) | Portuguese, Spanish, French, Hindi, Italian, Japanese |

```bash
cargo build --release --features speak
cargo build --release --features speak-phonemize   # includes speak
```

`piper-phonemize` is **GPL-3.0-or-later** (espeak-ng), which is why it is not
enabled by default. A `speak`-only build still runs for every language, but
non-English/Chinese voices fall back to an English/Chinese lexicon that does not
cover them, so they sound wrong. The TUI warns when you pick such a voice in a
build without the frontend.

## Model bundles

Both multilingual Kokoro releases load through the same code. `voices.bin`
contains the speaker embeddings; the bundle directory name selects the
published name table.

| Bundle directory | Speakers | Languages | Default |
| --- | ---: | --- | --- |
| `kokoro-multi-lang-v1_1` (or `kokoro-int8-multi-lang-v1_1`) | 103 | Chinese (100), English US (2), English UK (1) | Recommended |
| `kokoro-multi-lang-v1_0` | 54 | Chinese (8), English US (20), English UK (8), Spanish (3), French (1), Hindi (4), Italian (2), Japanese (5), **Portuguese (Brazil) (3)** | Required for non-English/Chinese voices |

Download and extract a bundle (`~` is a shell abbreviation only — the config
expects real paths):

```bash
curl -L -o kokoro.tar.bz2 \
  https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_0.tar.bz2
mkdir -p ~/.cache/morpheus/models
tar xf kokoro.tar.bz2 -C ~/.cache/morpheus/models
```

The directory must contain `model.onnx` (or `model.int8.onnx`), `tokens.txt`,
`voices.bin` and, for the lexicon frontend, at least one `lexicon-*.txt`.

**Which bundle does Morpheus load?**

1. `voice.model_dir`, when it is not empty — used verbatim (an absolute path is
   safest; `~` is *not* expanded);
2. otherwise `$MORPHEUS_KOKORO_MODEL_DIR`, when set;
3. otherwise `<cache>/morpheus/models/kokoro-multi-lang-v1_1`. If that
   directory does not exist but another complete bundle is installed beside it,
   that bundle is used instead (v1.1 first, then v1.0). This keeps a v1.0-only
   installation — the only one with Portuguese — working without an env var.

The TUI's voice picker lists the configured bundle **and every other complete
bundle beside it**, so picking a v1.0 Portuguese voice automatically points
`voice.model_dir` at the v1.0 directory.

Only the *automatic* path falls back. An explicit `voice.model_dir` (or
`MORPHEUS_KOKORO_MODEL_DIR`) that points at a deleted directory fails with
`model directory ... does not exist` instead of quietly loading another
bundle; the picker handles this case by listing the bundles that are really
installed, and confirming one rewrites `voice.model_dir`.

## Languages and frontends

sherpa-onnx 2.x no longer embeds espeak-ng. Its built-in frontend is a lexicon
table, and the multilingual bundles only ship lexicons for **English and
Chinese**. Every other language has voices in `voices.bin` but must be
phonemized externally before generation: espeak-ng (via `piper-phonemize`) for
Spanish, French, Hindi, Italian and Brazilian Portuguese, and a dictionary-based
Japanese frontend for Japanese. espeak-ng cannot read kanji (it spells them
out as “chinese letter”) and v1.0 was trained on misaki's cutlet phonemes, so
Japanese never goes through espeak-ng.

| Language | Code | Frontend | v1.0 voices | v1.1 voices |
| --- | --- | --- | ---: | ---: |
| English (US) | `en-us` | lexicon | 20 | 2 |
| English (UK) | `en-gb` | lexicon | 8 | 1 |
| Chinese | `zh` | lexicon | 8 | 100 |
| Portuguese (Brazil) | `pt-br` | espeak-ng | 3 | 0 |
| Spanish | `es` | espeak-ng | 3 | 0 |
| French | `fr` | espeak-ng | 1 | 0 |
| Hindi | `hi` | espeak-ng | 4 | 0 |
| Italian | `it` | espeak-ng | 2 | 0 |
| Japanese | `ja` | Japanese dictionary | 5 | 0 |

How the frontend is chosen at load time:

- `voice.language` non-empty — that espeak-ng language is used, except when
  it is the selected voice's own language **and** the bundle has a lexicon for
  it (`en-us`, `en-gb`, `zh`): those stay on the lexicon, the frontend the
  voices were built around. A mismatch is still honored (with a log warning),
  which is the manual escape hatch.
- `voice.language` empty — English/Chinese speakers keep the bundle lexicon;
  any other speaker is phonemized with the language its voice was trained for.
  This is why `"speaker": 42` alone is enough for `pf_dora` (`pt-br`), and why
  the picker stores an empty language when you choose an English or Chinese
  voice.

A speaker id and a language that disagree (for example `"speaker": 42` with
`"language": "en-us"`) produce the right voice with the wrong phonemes; the
daemon logs a warning naming both. Changing the picker writes a consistent
pair.

espeak-ng gets its complete `espeak-ng-data` from the bundle, not from the
minimal copy embedded in `piper-phonemize` (which lacks the per-language
dictionaries and made Spanish, French, Hindi and Italian synthesize silence).
Japanese kanji readings come from an embedded IPADIC dictionary; a mixed
Japanese/English sentence keeps ASCII words as written, matching misaki.

The frontend follows the **configured voice**, not the text being spoken:
there is no language detection per answer. An English answer read by `pf_dora`
uses the Portuguese phonemes, and vice versa, so a session whose answers come
in another language needs the speaker changed to one of that language (or the
text passes through as accented gibberish, not a slight accent). The daemon
logs the speaker, bundle and frontend it loads, which is the first thing to
check when the voice sounds wrong.

## Speakers

Every speaker has a stable published name. The prefix is the language and the
gender:

| Prefix | Meaning |
| --- | --- |
| `af` / `am` | English (US), female / male |
| `bf` / `bm` | English (UK), female / male |
| `ef` / `em` | Spanish, female / male |
| `ff` / `fm` | French, female / male |
| `hf` / `hm` | Hindi, female / male |
| `if` / `im` | Italian, female / male |
| `jf` / `jm` | Japanese, female / male |
| `pf` / `pm` | Portuguese (Brazil), female / male |
| `zf` / `zm` | Chinese, female / male |

The **speaker id is the position of the voice in its bundle**, not a global
code. It is passed to sherpa-onnx as `sid`, and it is only meaningful for the
bundle you load:

- id `42` is `pf_dora` (Portuguese, female) in v1.0 and `zf_075` (Chinese) in
  v1.1;
- id `43` is `pm_alex` (Portuguese, male) in v1.0 and `zf_076` in v1.1;
- id `3` is `af_heart` (English US) in v1.0 and `zf_001` (Chinese) in v1.1.

The TUI stores the id **together with** `language` and, when the voice belongs
to another installed bundle, `model_dir`. If you copy a speaker id from this
page into `setup.json`, set the bundle it came from. A renamed/unknown bundle
still accepts raw ids — only the names become unavailable.

### `kokoro-multi-lang-v1_1` (103 speakers)

| `sid` | Name | Language | Language label | Gender |
| ---: | --- | --- | --- | --- |
| `0` | `af_maple` | `en-us` | English (US) | female |
| `1` | `af_sol` | `en-us` | English (US) | female |
| `2` | `bf_vale` | `en-gb` | English (UK) | female |
| `3` | `zf_001` | `zh` | Chinese | female |
| `4` | `zf_002` | `zh` | Chinese | female |
| `5` | `zf_003` | `zh` | Chinese | female |
| `6` | `zf_004` | `zh` | Chinese | female |
| `7` | `zf_005` | `zh` | Chinese | female |
| `8` | `zf_006` | `zh` | Chinese | female |
| `9` | `zf_007` | `zh` | Chinese | female |
| `10` | `zf_008` | `zh` | Chinese | female |
| `11` | `zf_017` | `zh` | Chinese | female |
| `12` | `zf_018` | `zh` | Chinese | female |
| `13` | `zf_019` | `zh` | Chinese | female |
| `14` | `zf_021` | `zh` | Chinese | female |
| `15` | `zf_022` | `zh` | Chinese | female |
| `16` | `zf_023` | `zh` | Chinese | female |
| `17` | `zf_024` | `zh` | Chinese | female |
| `18` | `zf_026` | `zh` | Chinese | female |
| `19` | `zf_027` | `zh` | Chinese | female |
| `20` | `zf_028` | `zh` | Chinese | female |
| `21` | `zf_032` | `zh` | Chinese | female |
| `22` | `zf_036` | `zh` | Chinese | female |
| `23` | `zf_038` | `zh` | Chinese | female |
| `24` | `zf_039` | `zh` | Chinese | female |
| `25` | `zf_040` | `zh` | Chinese | female |
| `26` | `zf_042` | `zh` | Chinese | female |
| `27` | `zf_043` | `zh` | Chinese | female |
| `28` | `zf_044` | `zh` | Chinese | female |
| `29` | `zf_046` | `zh` | Chinese | female |
| `30` | `zf_047` | `zh` | Chinese | female |
| `31` | `zf_048` | `zh` | Chinese | female |
| `32` | `zf_049` | `zh` | Chinese | female |
| `33` | `zf_051` | `zh` | Chinese | female |
| `34` | `zf_059` | `zh` | Chinese | female |
| `35` | `zf_060` | `zh` | Chinese | female |
| `36` | `zf_067` | `zh` | Chinese | female |
| `37` | `zf_070` | `zh` | Chinese | female |
| `38` | `zf_071` | `zh` | Chinese | female |
| `39` | `zf_072` | `zh` | Chinese | female |
| `40` | `zf_073` | `zh` | Chinese | female |
| `41` | `zf_074` | `zh` | Chinese | female |
| `42` | `zf_075` | `zh` | Chinese | female |
| `43` | `zf_076` | `zh` | Chinese | female |
| `44` | `zf_077` | `zh` | Chinese | female |
| `45` | `zf_078` | `zh` | Chinese | female |
| `46` | `zf_079` | `zh` | Chinese | female |
| `47` | `zf_083` | `zh` | Chinese | female |
| `48` | `zf_084` | `zh` | Chinese | female |
| `49` | `zf_085` | `zh` | Chinese | female |
| `50` | `zf_086` | `zh` | Chinese | female |
| `51` | `zf_087` | `zh` | Chinese | female |
| `52` | `zf_088` | `zh` | Chinese | female |
| `53` | `zf_090` | `zh` | Chinese | female |
| `54` | `zf_092` | `zh` | Chinese | female |
| `55` | `zf_093` | `zh` | Chinese | female |
| `56` | `zf_094` | `zh` | Chinese | female |
| `57` | `zf_099` | `zh` | Chinese | female |
| `58` | `zm_009` | `zh` | Chinese | male |
| `59` | `zm_010` | `zh` | Chinese | male |
| `60` | `zm_011` | `zh` | Chinese | male |
| `61` | `zm_012` | `zh` | Chinese | male |
| `62` | `zm_013` | `zh` | Chinese | male |
| `63` | `zm_014` | `zh` | Chinese | male |
| `64` | `zm_015` | `zh` | Chinese | male |
| `65` | `zm_016` | `zh` | Chinese | male |
| `66` | `zm_020` | `zh` | Chinese | male |
| `67` | `zm_025` | `zh` | Chinese | male |
| `68` | `zm_029` | `zh` | Chinese | male |
| `69` | `zm_030` | `zh` | Chinese | male |
| `70` | `zm_031` | `zh` | Chinese | male |
| `71` | `zm_033` | `zh` | Chinese | male |
| `72` | `zm_034` | `zh` | Chinese | male |
| `73` | `zm_035` | `zh` | Chinese | male |
| `74` | `zm_037` | `zh` | Chinese | male |
| `75` | `zm_041` | `zh` | Chinese | male |
| `76` | `zm_045` | `zh` | Chinese | male |
| `77` | `zm_050` | `zh` | Chinese | male |
| `78` | `zm_052` | `zh` | Chinese | male |
| `79` | `zm_053` | `zh` | Chinese | male |
| `80` | `zm_054` | `zh` | Chinese | male |
| `81` | `zm_055` | `zh` | Chinese | male |
| `82` | `zm_056` | `zh` | Chinese | male |
| `83` | `zm_057` | `zh` | Chinese | male |
| `84` | `zm_058` | `zh` | Chinese | male |
| `85` | `zm_061` | `zh` | Chinese | male |
| `86` | `zm_062` | `zh` | Chinese | male |
| `87` | `zm_063` | `zh` | Chinese | male |
| `88` | `zm_064` | `zh` | Chinese | male |
| `89` | `zm_065` | `zh` | Chinese | male |
| `90` | `zm_066` | `zh` | Chinese | male |
| `91` | `zm_068` | `zh` | Chinese | male |
| `92` | `zm_069` | `zh` | Chinese | male |
| `93` | `zm_080` | `zh` | Chinese | male |
| `94` | `zm_081` | `zh` | Chinese | male |
| `95` | `zm_082` | `zh` | Chinese | male |
| `96` | `zm_089` | `zh` | Chinese | male |
| `97` | `zm_091` | `zh` | Chinese | male |
| `98` | `zm_095` | `zh` | Chinese | male |
| `99` | `zm_096` | `zh` | Chinese | male |
| `100` | `zm_097` | `zh` | Chinese | male |
| `101` | `zm_098` | `zh` | Chinese | male |
| `102` | `zm_100` | `zh` | Chinese | male |

### `kokoro-multi-lang-v1_0` (54 speakers)

| `sid` | Name | Language | Language label | Gender |
| ---: | --- | --- | --- | --- |
| `0` | `af_alloy` | `en-us` | English (US) | female |
| `1` | `af_aoede` | `en-us` | English (US) | female |
| `2` | `af_bella` | `en-us` | English (US) | female |
| `3` | `af_heart` | `en-us` | English (US) | female |
| `4` | `af_jessica` | `en-us` | English (US) | female |
| `5` | `af_kore` | `en-us` | English (US) | female |
| `6` | `af_nicole` | `en-us` | English (US) | female |
| `7` | `af_nova` | `en-us` | English (US) | female |
| `8` | `af_river` | `en-us` | English (US) | female |
| `9` | `af_sarah` | `en-us` | English (US) | female |
| `10` | `af_sky` | `en-us` | English (US) | female |
| `11` | `am_adam` | `en-us` | English (US) | male |
| `12` | `am_echo` | `en-us` | English (US) | male |
| `13` | `am_eric` | `en-us` | English (US) | male |
| `14` | `am_fenrir` | `en-us` | English (US) | male |
| `15` | `am_liam` | `en-us` | English (US) | male |
| `16` | `am_michael` | `en-us` | English (US) | male |
| `17` | `am_onyx` | `en-us` | English (US) | male |
| `18` | `am_puck` | `en-us` | English (US) | male |
| `19` | `am_santa` | `en-us` | English (US) | male |
| `20` | `bf_alice` | `en-gb` | English (UK) | female |
| `21` | `bf_emma` | `en-gb` | English (UK) | female |
| `22` | `bf_isabella` | `en-gb` | English (UK) | female |
| `23` | `bf_lily` | `en-gb` | English (UK) | female |
| `24` | `bm_daniel` | `en-gb` | English (UK) | male |
| `25` | `bm_fable` | `en-gb` | English (UK) | male |
| `26` | `bm_george` | `en-gb` | English (UK) | male |
| `27` | `bm_lewis` | `en-gb` | English (UK) | male |
| `28` | `ef_dora` | `es` | Spanish | female |
| `29` | `em_alex` | `es` | Spanish | male |
| `30` | `ff_siwis` | `fr` | French | female |
| `31` | `hf_alpha` | `hi` | Hindi | female |
| `32` | `hf_beta` | `hi` | Hindi | female |
| `33` | `hm_omega` | `hi` | Hindi | male |
| `34` | `hm_psi` | `hi` | Hindi | male |
| `35` | `if_sara` | `it` | Italian | female |
| `36` | `im_nicola` | `it` | Italian | male |
| `37` | `jf_alpha` | `ja` | Japanese | female |
| `38` | `jf_gongitsune` | `ja` | Japanese | female |
| `39` | `jf_nezumi` | `ja` | Japanese | female |
| `40` | `jf_tebukuro` | `ja` | Japanese | female |
| `41` | `jm_kumo` | `ja` | Japanese | male |
| `42` | `pf_dora` | `pt-br` | Portuguese (Brazil) | female |
| `43` | `pm_alex` | `pt-br` | Portuguese (Brazil) | male |
| `44` | `pm_santa` | `pt-br` | Portuguese (Brazil) | male |
| `45` | `zf_xiaobei` | `zh` | Chinese | female |
| `46` | `zf_xiaoni` | `zh` | Chinese | female |
| `47` | `zf_xiaoxiao` | `zh` | Chinese | female |
| `48` | `zf_xiaoyi` | `zh` | Chinese | female |
| `49` | `zm_yunjian` | `zh` | Chinese | male |
| `50` | `zm_yunxi` | `zh` | Chinese | male |
| `51` | `zm_yunxia` | `zh` | Chinese | male |
| `52` | `zm_yunyang` | `zh` | Chinese | male |
| `53` | `em_santa` | `es` | Spanish | male |

## Configuring `setup.json`

Voice settings live in the `voice` section. All fields are optional; missing
fields use the defaults below.

```json
{
  "voice": {
    "auto_generate": true,
    "speaker": 42,
    "speed": 1.0,
    "language": "pt-br",
    "model_dir": "/home/me/.cache/morpheus/models/kokoro-multi-lang-v1_0"
  }
}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `auto_generate` | `true` | Generate the final answer's audio at the end of every turn. Generation only: nothing is played until you press **speak**/`Ctrl+Y`. `false` generates on demand only. Legacy `enabled` is accepted as an alias. |
| `speaker` | `0` | Speaker id (`sid`) of the loaded bundle. See the tables above. Legacy `voice` is accepted as an alias. Use `0` for `af_alloy` in v1.0 / `af_maple` in v1.1. |
| `speed` | `1.0` | Pace factor. `1.0` is natural, **larger is faster**, smaller is slower. Values must be `> 0`; zero/NaN fall back to `1.0`. |
| `language` | `""` | espeak-ng language (`pt-br`, `en-us`, `es`, …). Empty derives it from `speaker` (non-English/Chinese voices) or uses the bundle lexicon (English/Chinese). A non-empty value wins, except when it is the voice's own language and the bundle has a lexicon for it (`en-us`, `en-gb`, `zh`): those keep the lexicon. |
| `model_dir` | `""` | Kokoro bundle directory. Empty resolves as described in [Model bundles](#model-bundles). An absolute path is safest; `~` is not expanded. |

Portuguese (Brazil), female voice. Needs the v1.0 bundle and
`speak-phonemize`; the language is derived from the speaker:

```json
{ "voice": { "speaker": 42, "model_dir": "/home/me/.cache/morpheus/models/kokoro-multi-lang-v1_0" } }
```

The same with an explicit language and a slower pace:

```json
{ "voice": { "speaker": 42, "language": "pt-br", "speed": 0.9,
             "model_dir": "/home/me/.cache/morpheus/models/kokoro-multi-lang-v1_0" } }
```

English (US), female (`af_heart`, v1.0), bundle lexicon, faster:

```json
{ "voice": { "speaker": 3, "speed": 1.15,
             "model_dir": "/home/me/.cache/morpheus/models/kokoro-multi-lang-v1_0" } }
```

Spanish, male (`em_alex`). The v1.1 bundle has no Spanish voice, so the v1.0
bundle must be selected:

```json
{ "voice": { "speaker": 29,
             "model_dir": "/home/me/.cache/morpheus/models/kokoro-multi-lang-v1_0" } }
```

### Applying changes

The daemon snapshots the `voice` section when it starts, because the model and
frontend are load-time settings. The TUI drains the daemon automatically after
a picker change or `/voice`. If you edit `setup.json` by hand, stop the running
daemon so the next synthesis starts a new one with the new settings:

```bash
pkill -f morpheus-ttsd        # Unix
# taskkill /IM morpheus-ttsd.exe /F    # Windows
```

The daemon exits on its own shortly after the last `morpheus` process that
used it closes, so a closed session never leaves a resident `morpheus-ttsd`
(which would keep the `.exe` locked against a rebuild on Windows). The idle
timer remains the fallback: five minutes with no client activity, overridable
with `MORPHEUS_TTSD_IDLE_SECS`.

## What the TUI provides

`/settings` → **Voice** (`speak` builds only):

| Row | Writes |
| --- | --- |
| Auto-generate | `voice.auto_generate` |
| Speaker | `voice.speaker`, `voice.language`, and `voice.model_dir` when the voice belongs to another installed bundle |
| Speech pace | `voice.speed` |

The **Speaker** row lists the configured bundle plus every other complete
bundle found beside it, grouped by language and bundle. The detail after each
name is `id · gender · frontend`, where `frontend` is `lexicon` (English and
Chinese), `Japanese dictionary` (Japanese) or `espeak-ng` (Spanish, French,
Hindi, Italian and Brazilian Portuguese).

The picker is searchable across name, id, language code/label, gender and
bundle; `/voice` toggles automatic generation.
`Ctrl+Y` (or the **speak** control after an answer) plays the last answer; older
answers keep their own **speak** control, and the playing clip offers
**pause**/**resume**/**rewind**.

The controls only exist when the binary was built with `speak`; the `voice`
section of `setup.json` is preserved either way.

## Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `model directory ... does not exist` | No bundle found (see [Model bundles](#model-bundles)). Download one into `<cache>/morpheus/models/`, or set `voice.model_dir`/`MORPHEUS_KOKORO_MODEL_DIR`. |
| `contains no lexicon-*.txt and the lexicon frontend needs one` | The bundle is incomplete (its `lexicon-*.txt` files are missing) or the model has no lexicon frontend. Use a complete official bundle, or build with `speak-phonemize`. |
| `voice id N is out of range` | `speaker` was copied from the other bundle. Check the id against the table of the bundle actually loaded. |
| Voice speaks with a foreign accent / wrong phonemes | `voice.language` does not match the speaker (see [Languages and frontends](#languages-and-frontends)). Set it to the speaker's language or leave it empty. |
| `piper-phonemize ... could not phonemize ... for language` | The build has no external frontend, the language code is invalid, or espeak-ng does not cover that text/language. Rebuild with `--features speak-phonemize`; check `voice.language`. Japanese uses its own frontend, so a Japanese failure means the text was empty or the IPADIC dictionary could not load, not an espeak-ng problem. |
| Japanese audio spells words or speaks “chinese letter” | The binary was built without `speak-phonemize`, so the Japanese voice fell back to the English lexicon. Rebuild with the feature; the Japanese frontend needs no model-side lexicon. |
| Audio is generated but nothing is heard | The system player is missing. Unix tries `paplay`, `pw-play`, `aplay`, `play`, then `ffplay`; macOS uses `afplay`/`ffplay`; Windows uses PowerShell `SoundPlayer`. Install one of them. |
| The WAV file exists in the session audio directory but no controls appear | The controls are tied to a finalized answer; the transcript entry must still be visible and the build must include `speak`. |

## Environment variables

| Variable | Effect |
| --- | --- |
| `MORPHEUS_KOKORO_MODEL_DIR` | Bundle directory when `voice.model_dir` is empty (overrides the cache default and the automatic fallback). |
| `MORPHEUS_TTSD_IDLE_SECS` | Idle seconds before the voice daemon exits (default `300`). |
| `MORPHEUS_CONFIG_DIR` | Location of `setup.json`. |
| `MORPHEUS_DATA_DIR` | Location of sessions and their audio files (`session-<id>.audio/closure-<id>.wav`). |

## Architecture notes

```text
final answer ──> TUI (SpeechState) ──tts/synthesize──> morpheus-ttsd
                                                        │
                             setup.json voice section ──┤ load once
                                                        ▼
                                          Kokoro + lexicon or espeak-ng
                                                        │
                                    WAV + bass envelope ◄┘
                     TUI stores `<sessions>/session-<id>.audio/closure-<id>.wav`
                     and plays it with the system player (pause/rewind)
```

- `src/speak` — the engine. `voice.rs` owns the published speaker tables and
  the language metadata; `backend/kokoro.rs` resolves the bundle files and the
  frontend; `config.rs`/`engine.rs` expose `SpeakConfig`/`Speak`.
- `src/daemon/tts` — the resident model, the wire protocol and the client.
  The daemon reads the `voice` section once; per-request overrides exist only
  for `voice` and `speed`.
- `src/tui` — the controls, the picker and playback. Playback lives in the TUI
  process so `pause`/`rewind` can signal the player.

Known limits:

- One frontend per synthesis: a mixed-language answer is spoken with the
  selected language's phonemes throughout (code snippets and English
  identifiers included).
- Model download is manual; the picker names only installed bundles.
- `speak-phonemize` is a compile-time choice; the daemon cannot switch to
  espeak-ng at runtime.
