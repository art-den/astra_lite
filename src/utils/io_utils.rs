#![allow(dead_code)]

use std::path::{PathBuf, Path};

pub fn save_json_to_config<T: serde::Serialize>(
    obj:       &T,
    conf_name: &str
) -> anyhow::Result<()> {
    let file_name = get_app_conf_file_name(conf_name, true)?;
    let options_str = serde_json::to_string_pretty(obj)?;
    std::fs::write(file_name, options_str)?;
    Ok(())
}

pub fn load_json_from_config_file<T: serde::de::DeserializeOwned>(
    obj:       &mut T,
    conf_name: &str
) -> anyhow::Result<()> {
    let file_name = get_app_conf_file_name(conf_name, false)?;
    if !file_name.is_file() { return Ok(()); }
    let file = std::io::BufReader::new(std::fs::File::open(file_name)?);
    *obj = serde_json::from_reader(file)?;
    Ok(())
}

pub fn get_app_dir() -> anyhow::Result<PathBuf> {
    let conf_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("dirs::config_dir()"))?;
    let mut path = PathBuf::from(&conf_dir);
    path.push(format!(".{}", env!("CARGO_PKG_NAME")));
    Ok(path)
}

fn get_app_conf_file_name(
    conf_name: &str,
    create_dir: bool
) -> anyhow::Result<PathBuf> {
    let mut path = get_app_dir()?;
    if create_dir && !path.exists() {
        std::fs::create_dir(&path)?;
    }
    path.push(format!("{}.json", conf_name));
    Ok(path)
}

pub struct SeqFileNameGen {
    last_num: u32,
}

impl SeqFileNameGen {
    pub fn new() -> Self {
        Self {
            last_num: 1,
        }
    }

    pub fn generate(&mut self, parent_path: &Path, file_mask: &str) -> PathBuf {
        loop {
            let num_str = format!("{:04}", self.last_num);
            let file_name = file_mask.replace("${num}", &num_str);
            let result = parent_path.join(file_name);
            self.last_num += 1;
            if !result.is_file() && !result.is_dir() {
                return result;
            }
        }
    }
}

pub fn get_free_folder_name(path: &Path) -> PathBuf {
    if !path.is_dir() && !path.is_file() { return path.to_path_buf(); }
    let name = path.file_name().unwrap_or_default().to_str().unwrap_or_default();
    let mut result = path.to_path_buf();
    for index in 2..1_000_000 {
        let new_name = format!("{}_{}", name, index);
        result.set_file_name(new_name);
        if !result.is_dir() && !result.is_file() { break; }
    }
    return result;
}

pub fn get_free_file_name(path: &Path) -> PathBuf {
    if !path.is_dir() && !path.is_file() { return path.to_path_buf(); }
    let stem = path.file_stem().unwrap_or_default().to_str().unwrap_or_default();
    let ext = path.extension().unwrap_or_default().to_str().unwrap_or_default();
    let mut result = path.to_path_buf();
    for index in 2..1_000_000 {
        let new_name = format!("{}_{}.{}", stem, index, ext);
        result.set_file_name(new_name);
        if !result.is_dir() && !result.is_file() { break; }
    }
    return result;
}
