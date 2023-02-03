use std::path::PathBuf;

pub fn save_json_to_config<T: serde::Serialize>(
    obj:       &T,
    conf_name: &str
) -> anyhow::Result<()> {
    let file_name = get_file_name(conf_name, true)?;
    let options_str = serde_json::to_string_pretty(obj)?;
    std::fs::write(file_name, options_str)?;
    Ok(())
}

pub fn load_json_from_config_file<T: serde::de::DeserializeOwned>(
    obj:       &mut T,
    conf_name: &str
) -> anyhow::Result<()> {
    let file_name = get_file_name(conf_name, false)?;
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

fn get_file_name(
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
