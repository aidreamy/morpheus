use super::super::{App, HOME_LOCK, isolate_home};
use crossterm::event::KeyCode;

/// Settings → Voice's picker: search by voice name/language, then Enter
/// applies speaker AND language from the same row. Esc cancels without
/// touching setup, and the pick persists to `setup.json`.
#[cfg(feature = "speak")]
#[tokio::test]
async fn voice_picker_applies_speaker_and_language_together() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.setup.voice.model_dir = "/models/kokoro-multi-lang-v1_0".into();
    app.setup.voice.speaker = 42;
    app.setup.voice.language = "pt-br".into();

    // Filter by the full voice name and confirm pm_alex: the language comes
    // along. (A bare "alex" also matches the Spanish `em_alex`, so the
    // filter keeps every language a name occurs in.)
    app.open_voice_dialog();
    for ch in "pm_alex".chars() {
        app.handle_voice_dialog_key(KeyCode::Char(ch));
    }
    assert!(app.handle_voice_dialog_key(KeyCode::Enter));
    assert!(!app.dialog.visible());
    assert_eq!(app.setup.voice.speaker, 43);
    assert_eq!(app.setup.voice.language, "pt-br");

    // Reopening preselects the configured voice: Enter without typing is a
    // no-op, never a switch to the first catalog row.
    app.open_voice_dialog();
    assert!(app.handle_voice_dialog_key(KeyCode::Enter));
    assert_eq!(app.setup.voice.speaker, 43);

    // A filter that matches every Portuguese voice starts at the first row
    // (the filter resets the selection); Esc leaves setup untouched.
    app.open_voice_dialog();
    for ch in "pt-br".chars() {
        app.handle_voice_dialog_key(KeyCode::Char(ch));
    }
    app.handle_voice_dialog_key(KeyCode::Down);
    app.handle_voice_dialog_key(KeyCode::Esc);
    assert_eq!(app.setup.voice.speaker, 43);

    // Down twice from the first Portuguese row lands on pm_santa (44).
    app.open_voice_dialog();
    for ch in "pt-br".chars() {
        app.handle_voice_dialog_key(KeyCode::Char(ch));
    }
    app.handle_voice_dialog_key(KeyCode::Down);
    app.handle_voice_dialog_key(KeyCode::Down);
    assert!(app.handle_voice_dialog_key(KeyCode::Enter));
    assert_eq!(app.setup.voice.speaker, 44);
    assert_eq!(app.setup.voice.language, "pt-br");
    let persisted = morpheus::setup::Setup::load();
    assert_eq!(persisted.voice.speaker, 44);

    // An English voice clears the language instead: the bundle's lexicon is
    // the frontend that voice was built around, and an explicit `en-us`
    // would route English through espeak-ng.
    app.open_voice_dialog();
    for ch in "af_heart".chars() {
        app.handle_voice_dialog_key(KeyCode::Char(ch));
    }
    assert!(app.handle_voice_dialog_key(KeyCode::Enter));
    assert_eq!(app.setup.voice.speaker, 3);
    assert_eq!(app.setup.voice.language, "");
    assert_eq!(app.setup.voice.model_dir, "/models/kokoro-multi-lang-v1_0");
}

/// A v1.1 installation has no Portuguese voice. The picker still lists the
/// v1.0 bundle installed beside it, and confirming one of its voices switches
/// `model_dir` - the user never edits `setup.json` to reach the languages the
/// recommended bundle lacks.
#[cfg(feature = "speak")]
#[tokio::test]
async fn picking_a_voice_from_another_installed_bundle_switches_the_bundle() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let models = tempfile::tempdir().unwrap();
    let bundle = |name: &str| {
        let dir = models.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for file in [
            "model.onnx",
            "tokens.txt",
            "voices.bin",
            "lexicon-us-en.txt",
        ] {
            std::fs::write(dir.join(file), b"").unwrap();
        }
        dir
    };
    let first = bundle("kokoro-multi-lang-v1_0");
    let second = bundle("kokoro-multi-lang-v1_1");

    let mut app = App::new("/tmp".to_string());
    app.setup.voice.model_dir = second.to_string_lossy().into_owned();
    app.setup.voice.speaker = 0;
    app.setup.voice.language = "en-us".into();

    app.open_voice_dialog();
    for ch in "pf_dora".chars() {
        app.handle_voice_dialog_key(KeyCode::Char(ch));
    }
    assert!(app.handle_voice_dialog_key(KeyCode::Enter));
    assert_eq!(app.setup.voice.speaker, 42);
    assert_eq!(app.setup.voice.language, "pt-br");
    assert_eq!(
        app.setup.voice.model_dir,
        first.to_string_lossy().into_owned()
    );

    // Reopening preselects the voice in its now-configured bundle.
    app.open_voice_dialog();
    assert!(app.handle_voice_dialog_key(KeyCode::Enter));
    assert_eq!(app.setup.voice.speaker, 42);
    assert_eq!(
        app.setup.voice.model_dir,
        first.to_string_lossy().into_owned()
    );
}
