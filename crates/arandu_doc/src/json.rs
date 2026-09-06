//! Pure JSON serializer for Arandu documentation models.

use arandu_middle::docs::DocModule;

/// Serializes a [`DocModule`] into a JSON string.
pub fn render_json(module: &DocModule, pretty: bool) -> Result<String, serde_json::Error> {
    if pretty {
        serde_json::to_string_pretty(module)
    } else {
        serde_json::to_string(module)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_empty_module() {
        let module = DocModule {
            name: "std.test".to_string(),
            path: "stdlib/test.aru".to_string(),
            file_id: 1,
            overview: Some("Module overview".to_string()),
            items: Vec::new(),
        };

        let json = render_json(&module, true).expect("json serialization");
        assert!(json.contains("\"name\": \"std.test\""));
        assert!(json.contains("\"overview\": \"Module overview\""));
    }
}
