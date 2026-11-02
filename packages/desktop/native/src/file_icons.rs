#[path = "file_icon_data.rs"]
mod data;

pub fn lookup(file_name: &str) -> (&'static str, &'static str) {
    let extension = file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_lowercase());
    let name = extension
        .as_deref()
        .and_then(data::extension_icon)
        .unwrap_or("_default");
    (name, data::svg(name))
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_the_authored_file_icon_selector() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("testdata/file-icons.json")).unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            let file = case[0].as_str().unwrap();
            assert_eq!(super::lookup(file).0, case[1].as_str().unwrap(), "{file}");
        }
    }
}
