//! Índice local de copias dentro de la carpeta elegida.
//!
//! `<carpeta>/.agest-backup/index.json` recuerda qué ZIP corresponde a qué
//! ejecución del servidor: hace falta para aplicar la retención (y avisar al
//! panel de que se borró) y para saber cuándo fue la última copia correcta de
//! cada tienda sin depender de la conexión.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub run_id: i64,
    pub shop_id: i64,
    pub shop_name: String,
    /// Ruta relativa a la carpeta de copias.
    pub file: String,
    pub size: u64,
    pub sha256: String,
    pub completed_at: DateTime<Local>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    #[serde(default)]
    pub entries: Vec<Entry>,
}

fn index_path(dir: &Path) -> PathBuf {
    dir.join(".agest-backup").join("index.json")
}

impl Index {
    /// Carga el índice y descarta las copias que ya no están en disco.
    pub fn load(dir: &Path) -> Self {
        let mut index: Index = std::fs::read_to_string(index_path(dir))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        index.entries.retain(|e| dir.join(&e.file).is_file());
        index.discover(dir);
        index
    }

    /// Añade los ZIP de las carpetas de tienda que no están en el índice
    /// (índice perdido o copiado de otro equipo), para no "olvidar" copias
    /// que siguen en disco. Llevan un `run_id` negativo: no son del servidor.
    fn discover(&mut self, dir: &Path) {
        let known: std::collections::HashSet<String> =
            self.entries.iter().map(|e| e.file.clone()).collect();
        let Ok(folders) = std::fs::read_dir(dir) else {
            return;
        };
        for folder in folders.flatten() {
            let folder_name = folder.file_name().to_string_lossy().to_string();
            let Some(shop_id) = parse_shop_folder(&folder_name) else {
                continue;
            };
            if !folder.path().is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(folder.path()) else {
                continue;
            };
            for file in files.flatten() {
                let name = file.file_name().to_string_lossy().to_string();
                if !name.starts_with("agest-") || !name.to_ascii_lowercase().ends_with(".zip") {
                    continue;
                }
                let relative = format!("{folder_name}/{name}");
                if known.contains(&relative) {
                    continue;
                }
                let Ok(meta) = file.metadata() else { continue };
                if !meta.is_file() {
                    continue;
                }
                let completed_at = parse_stamp(&name)
                    .or_else(|| meta.modified().ok().map(DateTime::<Local>::from))
                    .unwrap_or_else(Local::now);
                let shop_name = folder_name
                    .split_once('-')
                    .map(|(_, slug)| slug.to_string())
                    .unwrap_or_default();
                self.entries.push(Entry {
                    run_id: local_id(&relative),
                    shop_id,
                    shop_name,
                    file: relative,
                    size: meta.len(),
                    sha256: String::new(),
                    completed_at,
                });
            }
        }
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = index_path(dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            hide(parent);
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn add(&mut self, entry: Entry) {
        self.entries.retain(|e| e.run_id != entry.run_id);
        self.entries.push(entry);
    }

    pub fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.size).sum()
    }

    pub fn last_success(&self) -> HashMap<i64, DateTime<Local>> {
        let mut map: HashMap<i64, DateTime<Local>> = HashMap::new();
        for e in &self.entries {
            map.entry(e.shop_id)
                .and_modify(|t| *t = (*t).max(e.completed_at))
                .or_insert(e.completed_at);
        }
        map
    }

    pub fn for_shop(&self, shop_id: i64) -> Vec<&Entry> {
        let mut list: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| e.shop_id == shop_id)
            .collect();
        list.sort_by_key(|e| std::cmp::Reverse(e.completed_at));
        list
    }

    /// Copias de la tienda que sobran para quedarse con las `keep` más recientes.
    pub fn to_prune(&self, shop_id: i64, keep: u32) -> Vec<Entry> {
        self.for_shop(shop_id)
            .into_iter()
            .skip(keep.max(1) as usize)
            .cloned()
            .collect()
    }
}

/// `12-mi-tienda` → 12.
fn parse_shop_folder(name: &str) -> Option<i64> {
    let digits = name.split('-').next()?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// `agest-12-mi-tienda-20261126-030000.zip` → 2026-11-26 03:00:00.
fn parse_stamp(file: &str) -> Option<DateTime<Local>> {
    let stem = file.split('.').next()?;
    let mut parts = stem.rsplitn(3, '-');
    let time = parts.next()?;
    let date = parts.next()?;
    let naive =
        chrono::NaiveDateTime::parse_from_str(&format!("{date}{time}"), "%Y%m%d%H%M%S").ok()?;
    naive.and_local_timezone(Local).earliest()
}

/// Id estable y negativo para una copia que el servidor no conoce.
fn local_id(relative: &str) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in relative.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    -((h >> 1) as i64).max(1)
}

#[cfg(windows)]
fn hide(path: &Path) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("attrib")
        .arg("+h")
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

#[cfg(not(windows))]
fn hide(_path: &Path) {}

/// Nombre de carpeta seguro para una tienda: `12-mi-tienda`.
pub fn shop_folder(shop_id: i64, name: &str) -> String {
    let mut slug = String::new();
    for c in name.to_lowercase().chars() {
        let c = match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(40).collect();
    if slug.is_empty() {
        format!("{shop_id}")
    } else {
        format!("{shop_id}-{slug}")
    }
}

/// Espacio total y libre del disco que contiene `path`.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let probe = if path.exists() {
        path.to_path_buf()
    } else {
        path.parent()?.to_path_buf()
    };
    Some((
        fs2::total_space(&probe).ok()?,
        fs2::available_space(&probe).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn entry(run: i64, shop: i64, day: u32) -> Entry {
        Entry {
            run_id: run,
            shop_id: shop,
            shop_name: "Tienda".into(),
            file: format!("{shop}/copia-{run}.zip"),
            size: 100,
            sha256: "x".into(),
            completed_at: Local
                .with_ymd_and_hms(2026, 11, day, 3, 0, 0)
                .single()
                .unwrap(),
        }
    }

    #[test]
    fn retencion_deja_las_mas_recientes() {
        let mut idx = Index::default();
        for (run, day) in [(1, 1), (2, 2), (3, 3), (4, 4)] {
            idx.add(entry(run, 7, day));
        }
        idx.add(entry(9, 8, 1));
        let prune: Vec<i64> = idx.to_prune(7, 2).iter().map(|e| e.run_id).collect();
        assert_eq!(prune, vec![2, 1]);
        assert!(idx.to_prune(8, 2).is_empty());
        // nunca deja cero copias
        assert_eq!(idx.to_prune(7, 0).len(), 3);
    }

    #[test]
    fn ultima_copia_por_tienda_y_total() {
        let mut idx = Index::default();
        idx.add(entry(1, 7, 1));
        idx.add(entry(2, 7, 5));
        idx.add(entry(2, 7, 5)); // misma ejecución: no se duplica
        assert_eq!(idx.entries.len(), 2);
        assert_eq!(idx.total_bytes(), 200);
        assert_eq!(idx.last_success()[&7], entry(2, 7, 5).completed_at);
    }

    #[test]
    fn guarda_y_descarta_lo_que_falta_en_disco() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("7")).unwrap();
        std::fs::write(dir.path().join("7/copia-1.zip"), b"zip").unwrap();
        let mut idx = Index::default();
        idx.add(entry(1, 7, 1));
        idx.add(entry(2, 7, 2)); // su fichero no existe
        idx.save(dir.path()).unwrap();
        let loaded = Index::load(dir.path());
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].run_id, 1);
    }

    #[test]
    fn recupera_zips_fuera_del_indice() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("12-mi-tienda")).unwrap();
        std::fs::write(
            dir.path().join("12-mi-tienda/agest-12-mi-tienda-20261126-030000.zip"),
            b"zip",
        )
        .unwrap();
        std::fs::write(dir.path().join("12-mi-tienda/otra-cosa.zip"), b"x").unwrap();
        std::fs::create_dir_all(dir.path().join("fotos")).unwrap();
        std::fs::write(dir.path().join("fotos/agest-1.zip"), b"x").unwrap();
        let idx = Index::load(dir.path());
        assert_eq!(idx.entries.len(), 1);
        let e = &idx.entries[0];
        assert_eq!(e.shop_id, 12);
        assert!(e.run_id < 0);
        assert_eq!(e.size, 3);
        assert_eq!(
            e.completed_at.format("%Y%m%d-%H%M%S").to_string(),
            "20261126-030000"
        );
        // no se duplica si ya estaba indexada
        idx.save(dir.path()).unwrap();
        assert_eq!(Index::load(dir.path()).entries.len(), 1);
    }

    #[test]
    fn carpeta_de_tienda() {
        assert_eq!(
            shop_folder(12, "Panadería Núñez & Hijos"),
            "12-panaderia-nunez-hijos"
        );
        assert_eq!(shop_folder(3, "***"), "3");
    }
}
