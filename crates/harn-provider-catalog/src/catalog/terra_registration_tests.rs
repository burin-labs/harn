use super::*;

#[test]
fn terra_registration_reaches_packaged_routes_controls_and_picker() {
    let catalog = serde_json::to_value(artifact()).unwrap();
    let models = catalog["models"].as_array().unwrap();
    let model = |id: &str| {
        models
            .iter()
            .find(|model| model["id"] == id)
            .unwrap_or_else(|| panic!("missing packaged route {id}"))
    };
    for (id, provider) in [
        ("gpt-6.1-terra", "openai"),
        ("openai/gpt-6.1-terra", "openrouter"),
        ("openai/gpt-6.1-terra-pro", "openrouter"),
        ("vercel/openai/gpt-6.1-terra", "vercel_ai_gateway"),
        ("vercel/openai/gpt-6.1-terra-fast", "vercel_ai_gateway"),
    ] {
        let row = model(id);
        assert_eq!(row["provider"], provider);
        assert_eq!(row["context_window"], 1050000);
        assert_eq!(row["tool_support"]["native"], true);
        assert!(row["reasoning"]["effort_levels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|effort| effort == "max"));
    }
    assert_eq!(model("gpt-6.1-terra")["reasoning"]["none_supported"], true);
    assert_eq!(model("gpt-6.1-sol")["reasoning"]["none_supported"], false);
    assert_eq!(
        model("vercel/openai/gpt-6.1-terra")["reasoning"]["none_supported"],
        false
    );
    assert_eq!(
        model("openai/gpt-6.1-terra-pro")["reasoning"]["none_supported"],
        false
    );
    assert!(!models
        .iter()
        .any(|model| model["id"] == "gpt-6.1-terra-unserved"));
    assert!(catalog["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|alias| { alias["name"] == "terra61" && alias["model_id"] == "gpt-6.1-terra" }));
    let family = catalog["families"]
        .as_array()
        .unwrap()
        .iter()
        .find(|family| family["id"] == "openai-gpt-6")
        .unwrap();
    let variant = family["dimensions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|dimension| dimension["key"] == "variant")
        .unwrap();
    let choices = variant["ordered_values"].as_array().unwrap();
    let terra = choices
        .iter()
        .position(|v| v["model_id"] == "gpt-6.1-terra")
        .unwrap();
    let sol = choices
        .iter()
        .position(|v| v["model_id"] == "gpt-6.1-sol")
        .unwrap();
    assert_eq!(terra + 1, sol);
}
