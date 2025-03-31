pub fn clean_name(name: &str) -> String {
    name.trim().to_lowercase().replace(" ", "_")
}

