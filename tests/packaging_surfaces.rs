use serde_json::Value as JsonValue;
use std::fs;
use std::path::PathBuf;
use toml::Value as TomlValue;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &str) -> String {
    fs::read_to_string(repo_root().join(path)).expect("read file")
}

fn cargo_version() -> String {
    let cargo: TomlValue = toml::from_str(&read("Cargo.toml")).expect("parse Cargo.toml");
    cargo["package"]["version"]
        .as_str()
        .expect("cargo version")
        .to_string()
}

fn extract_quoted_value(source: &str, key: &str) -> String {
    source
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            let prefix = format!("{key} = ");
            trimmed
                .strip_prefix(&prefix)
                .map(|rest| rest.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| panic!("missing key: {key}"))
}

#[test]
fn package_versions_match_current_source_version() {
    let cargo_version = cargo_version();

    let npm_package: JsonValue =
        serde_json::from_str(&read("npm/package.json")).expect("parse npm/package.json");
    assert_eq!(
        npm_package["version"],
        JsonValue::String(cargo_version.clone())
    );

    let pyproject: TomlValue =
        toml::from_str(&read("python/pyproject.toml")).expect("parse python/pyproject.toml");
    assert_eq!(
        pyproject["project"]["version"].as_str(),
        Some(cargo_version.as_str())
    );

    let python_init = read("python/thronglets/__init__.py");
    assert_eq!(
        extract_quoted_value(&python_init, "__version__"),
        cargo_version
    );
    assert!(
        python_init
            .contains("VERSION = os.environ.get(\"THRONGLETS_INSTALL_VERSION\", __version__)")
    );
}

#[test]
fn published_server_manifest_is_internally_consistent() {
    let server: JsonValue = serde_json::from_str(&read("server.json")).expect("parse server.json");
    let version = server["version"].as_str().expect("server version");
    let packages = server["packages"].as_array().expect("server packages");

    assert!(!packages.is_empty(), "server.json should declare packages");
    for package in packages {
        assert_eq!(package["version"].as_str(), Some(version));
        let identifier = package["identifier"].as_str().expect("package identifier");
        assert!(
            identifier.contains(&format!("/v{version}/")),
            "identifier should embed manifest version: {identifier}"
        );
    }
}

#[test]
fn package_and_agent_docs_do_not_regress_to_old_context_model() {
    let docs = [
        ("npm/README.md", read("npm/README.md")),
        ("python/README.md", read("python/README.md")),
        ("docs/llms.txt", read("docs/llms.txt")),
    ];

    for (path, content) in docs {
        assert!(
            !content.contains("8 layers") && !content.contains("8 层"),
            "{path} regressed to the old 8-layer framing"
        );
        assert!(
            content.contains("thronglets setup"),
            "{path} should include the setup path"
        );
    }

    let npm_readme = read("npm/README.md");
    assert!(npm_readme.contains("avoid"));
    assert!(npm_readme.contains("thronglets bootstrap --agent codex --json"));

    let python_readme = read("python/README.md");
    assert!(python_readme.contains("thronglets install-plan --agent generic --json"));

    let llms = read("docs/llms.txt");
    assert!(llms.contains("thronglets.bootstrap.v2"));
    assert!(llms.contains("thronglets release-check --eval-scope both --json"));
}

#[test]
fn package_installers_read_version_from_a_single_source() {
    let npm_installer = read("npm/scripts/install.js");
    assert!(npm_installer.contains("THRONGLETS_INSTALL_VERSION"));
    assert!(npm_installer.contains("require(\"../package.json\")"));

    let python_installer = read("python/thronglets/__init__.py");
    assert!(python_installer.contains("THRONGLETS_INSTALL_VERSION"));
    assert!(python_installer.contains("THRONGLETS_INSTALL_REPO"));
}
