//! The images of a `--dir` and the order they are shown in.

use std::{
    hash::BuildHasher,
    path::{Path, PathBuf},
};

const EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

/// Sorted image files of `dir`, re-read on every call so new files are picked up.
pub fn list_images(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        })
        .collect();
    files.sort();
    files
}

/// The file after `current` in `files` (or the first), wrapping around.
pub fn next_image(files: &[PathBuf], current: Option<&Path>) -> Option<PathBuf> {
    let next = current
        .and_then(|c| files.iter().position(|f| f.as_path() == c))
        .map_or(0, |i| (i + 1) % files.len());
    files.get(next).cloned()
}

/// A random file of `files` other than `current`, unless it is the only one.
pub fn random_image(files: &[PathBuf], current: Option<&Path>) -> Option<PathBuf> {
    let others: Vec<&PathBuf> = files
        .iter()
        .filter(|f| Some(f.as_path()) != current)
        .collect();
    let pool = if others.is_empty() {
        files.iter().collect()
    } else {
        others
    };
    // std's randomly seeded hasher is random enough to pick a wallpaper, no RNG crate needed.
    let i = std::hash::RandomState::new().hash_one(()) as usize % pool.len().max(1);
    pool.get(i).map(|f| f.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_through_dir() {
        let dir = std::env::temp_dir().join(format!("reel-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["b.png", "a.JPG", "notes.txt"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let files = list_images(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        let first = next_image(&files, None).unwrap();
        let second = next_image(&files, Some(&first)).unwrap();
        assert_eq!(first.file_name().unwrap(), "a.JPG");
        assert_eq!(second.file_name().unwrap(), "b.png");
        assert_eq!(next_image(&files, Some(&second)).unwrap(), first);
        assert_eq!(next_image(&[], None), None);
    }

    #[test]
    fn shuffle_never_repeats_current() {
        let files: Vec<PathBuf> = ["a.png", "b.png", "c.png"].map(PathBuf::from).into();
        for _ in 0..100 {
            let pick = random_image(&files, Some(&files[1])).unwrap();
            assert!(pick != files[1] && files.contains(&pick));
        }
        assert_eq!(
            random_image(&files[..1], Some(&files[0])),
            Some(files[0].clone())
        );
        assert_eq!(random_image(&[], None), None);
    }
}
