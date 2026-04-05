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
    assert!(
        npm_package["os"]
            .as_array()
            .expect("npm os list")
            .iter()
            .any(|value| value == "win32"),
        "npm package should support win32"
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
        ("llms.txt", read("llms.txt")),
    ];

    for (path, content) in docs {
        assert!(
            !content.contains("8 layers") && !content.contains("8 层"),
            "{path} regressed to the old 8-layer framing"
        );
        assert!(
            content.contains("thronglets start"),
            "{path} should include the default onboarding path"
        );
    }

    let npm_readme = read("npm/README.md");
    assert!(npm_readme.contains("avoid"));
    assert!(npm_readme.contains("thronglets bootstrap --agent codex --json"));

    let python_readme = read("python/README.md");
    assert!(python_readme.contains("thronglets install-plan --agent generic --json"));
    assert!(python_readme.contains("thronglets share"));
    assert!(python_readme.contains("thronglets join"));
    assert!(python_readme.contains("thronglets status"));

    assert!(npm_readme.contains("thronglets share"));
    assert!(npm_readme.contains("thronglets join"));
    assert!(npm_readme.contains("thronglets status"));

    let llms = read("llms.txt");
    assert!(llms.contains("thronglets.bootstrap.v2"));
    assert!(llms.contains("thronglets release-check --eval-scope both --json"));

    for (path, content) in [
        ("README.md", read("README.md")),
        ("README.en.md", read("README.en.md")),
        ("llms.txt", read("llms.txt")),
    ] {
        assert!(
            content.contains("npm install -g thronglets")
                || content.contains("install.ps1")
                || content.contains("install.sh"),
            "{path} should point users at the prebuilt install surface"
        );
        let install_regressed = content.contains("cargo install thronglets")
            && !content.contains("do not")
            && !content.contains("不应该")
            && !content.contains("Development-only source path");
        assert!(
            !install_regressed,
            "{path} should not present cargo install as the default user install path"
        );
    }

    for (path, content) in [
        ("README.md", read("README.md")),
        ("README.en.md", read("README.en.md")),
        ("llms.txt", read("llms.txt")),
    ] {
        assert!(
            content.contains("thronglets start"),
            "{path} should teach the high-level first-device flow"
        );
        assert!(
            content.contains("thronglets join"),
            "{path} should teach the high-level second-device flow"
        );
        assert!(
            content.contains("thronglets share"),
            "{path} should teach the high-level primary-device sharing flow"
        );
    }

    let agents = read("AGENTS.md");
    assert!(agents.contains("thronglets share"));
    assert!(agents.contains("thronglets join             # join another node (reads the Desktop handoff file by default)"));

    let llms = read("llms.txt");
    let share_pos = llms.find("thronglets share").expect("llms share path");
    let join_pos = llms.find("thronglets join").expect("llms join path");
    assert!(
        share_pos < join_pos,
        "llms quick setup should teach primary-device share before second-device join"
    );

    let docs_index = read("docs/index.html");
    assert!(docs_index.contains("thronglets start"));
    assert!(docs_index.contains("thronglets share"));
    assert!(docs_index.contains("thronglets join"));
    assert!(
        docs_index.contains("'interface.setup.title': 'Prebuilt Install'"),
        "docs/index.html should keep its i18n setup title aligned with the public install surface"
    );
    assert!(
        !docs_index.contains("thronglets setup       <span class=\"cmt\"># install 6 hooks into Claude Code</span>"),
        "docs/index.html should not present setup as the normal public CLI path"
    );
    assert!(
        !docs_index.contains("'hero.alts': 'six hooks, zero config'"),
        "docs/index.html should not keep setup-first hero alt text in the i18n table"
    );
}

#[test]
fn package_installers_read_version_from_a_single_source() {
    let npm_installer = read("npm/scripts/install.js");
    assert!(npm_installer.contains("THRONGLETS_INSTALL_VERSION"));
    assert!(npm_installer.contains("require(\"../package.json\")"));
    assert!(npm_installer.contains("win32-x64"));
    assert!(npm_installer.contains("thronglets-mcp-windows-amd64.exe"));

    let npm_bin = read("npm/bin/thronglets.js");
    assert!(npm_bin.contains("execFileSync(binPath"));
    assert!(!npm_bin.contains("THRONGLETS_REPO_ROOT"));
    assert!(!npm_bin.contains("findRepoRoot"));
    assert!(!npm_bin.contains("cargo, [\"run\", \"--quiet\", \"--manifest-path\""));

    let python_installer = read("python/thronglets/__init__.py");
    assert!(python_installer.contains("THRONGLETS_INSTALL_VERSION"));
    assert!(python_installer.contains("THRONGLETS_INSTALL_REPO"));
    assert!(python_installer.contains("(\"Windows\", \"AMD64\")"));
}

#[test]
fn shell_installer_and_release_workflow_exist_for_one_line_distribution() {
    let install_script = read("scripts/install.sh");
    assert!(install_script.contains("releases/latest/download"));
    assert!(install_script.contains("THRONGLETS_VERSION"));
    assert!(install_script.contains("Next: thronglets start"));
    assert!(install_script.contains("scripts/install.ps1"));
    assert!(install_script.contains("thronglets-bin"));
    assert!(!install_script.contains("THRONGLETS_REPO_ROOT"));
    assert!(!install_script.contains("cargo run --quiet --manifest-path"));

    let powershell_installer = read("scripts/install.ps1");
    assert!(powershell_installer.contains("releases/latest/download"));
    assert!(powershell_installer.contains("thronglets-mcp-windows-amd64.exe"));
    assert!(powershell_installer.contains("Next: thronglets start"));
    assert!(powershell_installer.contains("thronglets-bin.exe"));
    assert!(!powershell_installer.contains("THRONGLETS_REPO_ROOT"));
    assert!(!powershell_installer.contains("cargo.exe"));

    let release_workflow = read(".github/workflows/release.yml");
    assert!(release_workflow.contains("softprops/action-gh-release"));
    assert!(release_workflow.contains("thronglets-mcp-linux-amd64"));
    assert!(release_workflow.contains("thronglets-mcp-darwin-arm64"));
    assert!(release_workflow.contains("thronglets-mcp-windows-amd64.exe"));
}
