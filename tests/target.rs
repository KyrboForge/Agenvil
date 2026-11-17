//! Model/effort resolution over a standalone project: no POLICY.md, defaults, bin/ or any other
//! Team Agents Kit file, only `agenvil.toml`, an agent, and standard Agent Skills.

use std::fs;

use agenvil::config::Config;
use agenvil::discovery::Limits;
use agenvil::inventory::Inventory;
use agenvil::target;
use serde_json::json;
use tempfile::TempDir;

mod common;
use common::agenvil;

const CONFIG: &str = "schema = 1\n[project]\nname = \"standalone\"\n\n[policy.models.claude]\nmodels = [\"sonnet\", \"opus\"]\nefforts = [\"medium\", \"high\", \"xhigh\"]\npairs = [[\"sonnet\", \"medium\"], [\"opus\", \"high\"], [\"opus\", \"xhigh\"]]\n\n[[targets.claude.map]]\nsource = [\"opus\", \"xhigh\"]\ntarget = [\"opus\", \"max\"]\n";

fn write(dir: &TempDir, path: &str, content: &str) {
    let full = dir.path().join(path);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, content).unwrap();
}

fn project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(&dir, "agenvil.toml", CONFIG);
    write(
        &dir,
        "agents/planner.md",
        "---\nname: planner\ndescription: Plans\nmodel: opus\neffort: high\nx-team: platform\n---\nBody\n",
    );
    write(
        &dir,
        "agents/writer.md",
        "---\nname: writer\ndescription: Writes\nmodel: sonnet\neffort: medium\n---\nBody\n",
    );
    write(
        &dir,
        "agents/deep.md",
        "---\nname: deep\ndescription: Thinks\nmodel: opus\neffort: xhigh\n---\nBody\n",
    );
    write(
        &dir,
        "agents/cheap.md",
        "---\nname: cheap\ndescription: Greps\nmodel: haiku\neffort: low\n---\nBody\n",
    );
    write(
        &dir,
        "agents/bare.md",
        "---\nname: bare\ndescription: Inherits\n---\nBody\n",
    );
    write(
        &dir,
        "agents/solo.md",
        "---\nname: solo\ndescription: Picks a model only\nmodel: sonnet\n---\nBody\n",
    );
    write(
        &dir,
        "agents/empty.md",
        "---\nname: empty\ndescription: Empty keys\nmodel:\neffort:\n---\nBody\n",
    );
    write(
        &dir,
        "skills/tdd/SKILL.md",
        "---\nname: tdd\ndescription: Test first\nlicense: MIT\nmetadata:\n  owner: me\n---\nBody\n",
    );
    dir
}

#[test]
fn resolves_a_standalone_project_deterministically() {
    let dir = project();
    let config = Config::load(dir.path()).unwrap();
    let inventory = Inventory::build(dir.path(), &config, &Limits::default()).unwrap();
    assert!(inventory.problems.is_empty(), "{:?}", inventory.problems);

    let results: Vec<_> = inventory
        .agents
        .iter()
        .chain(&inventory.skills)
        .map(|resource| {
            let fm = resource.frontmatter.as_ref().unwrap();
            let outcome = target::claude(&config, fm.get("model"), fm.get("effort"));
            let value = match outcome {
                Ok(resolution) => serde_json::to_value(resolution).unwrap(),
                Err(diagnostic) => serde_json::to_value(diagnostic).unwrap(),
            };
            (resource.path.clone(), value)
        })
        .collect();

    assert_eq!(
        serde_json::to_value(&results).unwrap(),
        json!([
            ["agents/bare.md", {"status": "inherit"}],
            ["agents/cheap.md", {"code": "source-not-allowed", "message": "model `haiku` is not allowed; add it to [policy.models.claude].models or change the source"}],
            ["agents/deep.md", {"status": "resolved", "source": ["opus", "xhigh"], "target": ["opus", "max"], "via": "override"}],
            ["agents/empty.md", {"status": "inherit"}],
            ["agents/planner.md", {"status": "resolved", "source": ["opus", "high"], "target": ["opus", "high"], "via": "same"}],
            ["agents/solo.md", {"status": "partial", "model": "sonnet"}],
            ["agents/writer.md", {"status": "resolved", "source": ["sonnet", "medium"], "target": ["sonnet", "medium"], "via": "same"}],
            // A standard SKILL.md declares neither field and is not asked to.
            ["skills/tdd/SKILL.md", {"status": "inherit"}],
        ])
    );

    // Unknown metadata survives untouched.
    let planner = inventory
        .agents
        .iter()
        .find(|a| a.name == "planner")
        .unwrap();
    assert_eq!(
        planner.frontmatter.as_ref().unwrap().get("x-team"),
        Some(&json!("platform"))
    );
    assert!(planner.source.contains("x-team: platform"));
}

#[test]
fn standalone_project_passes_the_model_rules_without_kit_files() {
    let dir = project();
    // Drop the agents that are meant to fail above; the rest needs no POLICY.md.
    fs::remove_file(dir.path().join("agents/cheap.md")).unwrap();
    agenvil(&dir)
        .args([
            "check", "--only", "AG002", "--only", "AG003", "--only", "AG004",
        ])
        .assert()
        .success();
}

#[test]
fn invalid_policy_is_a_usage_error_with_every_problem() {
    let dir = project();
    write(
        &dir,
        "agenvil.toml",
        "schema = 1\n[project]\nname = \"x\"\n[policy.models.claude]\nefforts = [\"high\"]\npairs = [[\"opus\", \"nope\"]]\n[[targets.claude.map]]\nsource = [\"opus\", \"high\"]\ntarget = [\"gpt-5\", \"high\"]\n",
    );
    let output = agenvil(&dir).args(["check"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    for needle in [
        "opus/nope uses an effort missing from `efforts`",
        "`gpt-5` is not a Claude model alias",
    ] {
        assert!(stderr.contains(needle), "{needle}\n{stderr}");
    }
}
