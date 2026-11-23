//! Uliana mode: the secret skin switch (D24), its sounds and the asset download (D32). The skin itself lives in the
//! ui crate.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::platform::System;

pub const ENV_VAR: &str = "ULIANA_MODE";

fn is_on(value: &str) -> bool {
    value.trim() == "1"
}

/// The process environment wins; without it, the user's persistent environment (autostart, pre-`setx` parents).
pub(crate) fn mode_enabled(system: &dyn System) -> bool {
    match std::env::var(ENV_VAR) {
        Ok(value) => is_on(&value),
        Err(_) => system
            .user_env_var(ENV_VAR)
            .is_some_and(|value| is_on(&value)),
    }
}

/// A `<prefix>*.wav` file from `dir`, chosen by `seed`. `None` when there is none.
pub(crate) fn pick_sound(dir: &Path, prefix: &str, seed: usize) -> Option<PathBuf> {
    let mut sounds: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            name.starts_with(prefix) && name.ends_with(".wav") && path.is_file()
        })
        .collect();
    sounds.sort();
    if sounds.is_empty() {
        return None;
    }
    let index = seed % sounds.len();
    Some(sounds.swap_remove(index))
}

/// The cat picture (Minecraft Wiki). `format=original` asks for the PNG instead of WebP.
const CAT_URL: &str = "https://static.wikia.nocookie.net/minecraft_gamepedia/images/2/21/Lying_down_Calico_Cat_with_Red_Collar.png/revision/latest/scale-to-width-down/1000?cb=20190824054036&format=original";
const MAX_CAT_BYTES: usize = 8 * 1024 * 1024;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Minecraft sound asset (under `minecraft/sounds/mob/`) → file name in the uliana folder.
const SOUNDS: [(&str, &str); 12] = [
    ("cat/meow1", "cat-meow1"),
    ("cat/meow2", "cat-meow2"),
    ("cat/meow3", "cat-meow3"),
    ("cat/meow4", "cat-meow4"),
    ("cat/purr1", "cat-purr1"),
    ("cat/purr2", "cat-purr2"),
    ("cat/purr3", "cat-purr3"),
    ("cat/purreow1", "cat-purreow1"),
    ("cat/purreow2", "cat-purreow2"),
    ("pig/say1", "pig-say1"),
    ("pig/say2", "pig-say2"),
    ("pig/say3", "pig-say3"),
];

/// Downloads the cat picture. HTTPS, no redirects, PNG only.
pub(crate) async fn download_cat() -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        // Without a user agent the wiki answers with a bot check page.
        .user_agent(concat!("zapret-gui/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(CAT_URL)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| error.to_string())?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CAT_BYTES as u64)
    {
        return Err("картинка слишком большая".into());
    }
    let bytes = response.bytes().await.map_err(|error| error.to_string())?;
    if bytes.len() > MAX_CAT_BYTES || !bytes.starts_with(PNG_SIGNATURE) {
        return Err("вместо картинки пришло что-то другое".into());
    }
    Ok(bytes.to_vec())
}

#[derive(Deserialize)]
struct AssetIndex {
    objects: HashMap<String, AssetObject>,
}

#[derive(Deserialize)]
struct AssetObject {
    hash: String,
}

/// The hash ends up in a path: exactly 40 lowercase hex digits.
fn is_valid_hash(hash: &str) -> bool {
    hash.len() == 40
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Asset hashes of [`SOUNDS`] from the newest asset index that has them.
fn sound_hashes(assets: &Path) -> Option<HashMap<&'static str, String>> {
    let mut indexes: Vec<(std::time::SystemTime, PathBuf)> =
        std::fs::read_dir(assets.join("indexes"))
            .ok()?
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                let modified = path.metadata().ok()?.modified().ok()?;
                (path.extension()? == "json").then_some((modified, path))
            })
            .collect();
    indexes.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    indexes.into_iter().find_map(|(_, path)| {
        let mut index: AssetIndex = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        let hashes: HashMap<&'static str, String> = SOUNDS
            .iter()
            .filter_map(|(name, _)| {
                let object = index
                    .objects
                    .remove(&format!("minecraft/sounds/mob/{name}.ogg"))?;
                is_valid_hash(&object.hash).then_some((*name, object.hash))
            })
            .collect();
        (!hashes.is_empty()).then_some(hashes)
    })
}

/// 16-bit PCM WAV, what `PlaySound` plays.
fn wav_bytes(channels: u16, sample_rate: u32, samples: &[i16]) -> Vec<u8> {
    let data_length = (samples.len() * 2) as u32;
    let block_align = channels * 2;
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_length).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * u32::from(block_align)).to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_length.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn ogg_to_wav(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut reader = lewton::inside_ogg::OggStreamReader::new(std::io::BufReader::new(file))
        .map_err(|error| error.to_string())?;
    let mut samples = Vec::new();
    while let Some(packet) = reader
        .read_dec_packet_itl()
        .map_err(|error| error.to_string())?
    {
        samples.extend(packet);
    }
    Ok(wav_bytes(
        u16::from(reader.ident_hdr.audio_channels),
        reader.ident_hdr.audio_sample_rate,
        &samples,
    ))
}

/// Converts the cat and pig sounds from a Minecraft install into WAVs in `out`. Returns how many were written.
pub(crate) fn install_sounds(assets: &Path, out: &Path) -> Result<usize, String> {
    let hashes = sound_hashes(assets).ok_or_else(|| {
        "Minecraft не найден. Установи его и запусти хотя бы раз, потом нажми снова.".to_string()
    })?;
    std::fs::create_dir_all(out).map_err(|error| error.to_string())?;
    let mut written = 0;
    for (name, file_name) in SOUNDS {
        let Some(hash) = hashes.get(name) else {
            continue;
        };
        let source = assets.join("objects").join(&hash[..2]).join(hash);
        match ogg_to_wav(&source).and_then(|wav| {
            std::fs::write(out.join(format!("{file_name}.wav")), wav)
                .map_err(|error| error.to_string())
        }) {
            Ok(()) => written += 1,
            Err(error) => tracing::warn!("uliana: sound {name} from {}: {error}", source.display()),
        }
    }
    if written == 0 {
        return Err("звуки Minecraft не получилось прочитать".into());
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_turns_it_on() {
        assert!(is_on("1"));
        assert!(is_on(" 1\n"));
        assert!(!is_on("0"));
        assert!(!is_on(""));
        assert!(!is_on("true"));
    }

    #[test]
    fn picks_only_matching_wavs() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "cat-meow1.wav",
            "cat-purr1.wav",
            "pig-say1.wav",
            "cat.png",
            "cat-meow2.ogg",
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let picked: Vec<String> = (0..4)
            .filter_map(|seed| pick_sound(dir.path(), "cat", seed))
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            picked,
            [
                "cat-meow1.wav",
                "cat-purr1.wav",
                "cat-meow1.wav",
                "cat-purr1.wav"
            ]
        );
        assert_eq!(
            pick_sound(dir.path(), "pig", 7)
                .unwrap()
                .file_name()
                .unwrap(),
            "pig-say1.wav"
        );
        assert!(pick_sound(dir.path(), "cow", 0).is_none());
        assert!(pick_sound(&dir.path().join("missing"), "cat", 0).is_none());
    }

    #[test]
    fn finds_sound_hashes_in_the_index() {
        let assets = tempfile::tempdir().unwrap();
        let indexes = assets.path().join("indexes");
        std::fs::create_dir_all(&indexes).unwrap();
        let good = "0123456789abcdef0123456789abcdef01234567";
        std::fs::write(
            indexes.join("17.json"),
            format!(
                r#"{{"objects": {{
                    "minecraft/sounds/mob/cat/meow1.ogg": {{"hash": "{good}", "size": 1}},
                    "minecraft/sounds/mob/pig/say1.ogg": {{"hash": "../../evil", "size": 1}},
                    "minecraft/sounds/mob/cow/say1.ogg": {{"hash": "{good}", "size": 1}}
                }}}}"#
            ),
        )
        .unwrap();
        let hashes = sound_hashes(assets.path()).unwrap();
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes["cat/meow1"], good);
        assert!(sound_hashes(&assets.path().join("missing")).is_none());
        assert!(install_sounds(&assets.path().join("missing"), assets.path()).is_err());
    }

    #[test]
    fn writes_a_pcm_wav_header() {
        let wav = wav_bytes(1, 22050, &[1, -1]);
        assert_eq!(wav.len(), 48);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 40);
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 44100);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 4);
        assert_eq!(&wav[44..], &[1, 0, 0xff, 0xff]);
    }
}
