use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tempfile::TempDir;

pub struct Fixture {
    project: TempDir,
    home: TempDir,
    pub claude: PathBuf,
    pub local: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let project = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let claude = home.path().join("claude-for-test");
        let local = home.path().join(".agents.local");
        put(
            &project.path().join("agenvil.toml"),
            "schema = 1\n[project]\nname = 'lifecycle-test'\n",
        );
        put(&project.path().join("rules/a.md"), "# Rule A\n");
        put(&project.path().join("rules/b.md"), "# Rule B\n");
        Self {
            project,
            home,
            claude,
            local,
        }
    }

    pub fn project(&self) -> &Path {
        self.project.path()
    }

    pub fn run(&self, command: &str, args: &[&str]) -> assert_cmd::assert::Assert {
        crate::common::agenvil_in(self.project(), self.home.path())
            .env("CLAUDE_CONFIG_DIR", &self.claude)
            .env("PATH", self.home.path().join("bin"))
            .args([command, "--target", "claude"])
            .args(args)
            .assert()
    }

    pub fn build(&self) {
        self.run("build", &[]).success();
    }

    pub fn sync(&self) {
        self.run("sync", &["--apply"]).success();
    }

    pub fn settings(&self) -> PathBuf {
        self.claude.join("settings.json")
    }

    pub fn install_settings(&self, fragment: &Value) {
        let path = self.project().join("fragment.json");
        put(&path, fragment.to_string());
        self.run(
            "settings",
            &["--fragment", path.to_str().unwrap(), "--apply"],
        )
        .success();
    }

    pub fn snapshot(&self) -> Vec<(PathBuf, Node)> {
        fn walk(dir: &Path, base: &Path, nodes: &mut Vec<(PathBuf, Node)>) {
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let kind = entry.file_type().unwrap();
                let node = if kind.is_symlink() {
                    Node::Link(fs::read_link(&path).unwrap())
                } else if kind.is_dir() {
                    walk(&path, base, nodes);
                    Node::Directory
                } else {
                    Node::File(fs::read(&path).unwrap())
                };
                nodes.push((path.strip_prefix(base).unwrap().to_owned(), node));
            }
        }
        let mut nodes = Vec::new();
        walk(self.home.path(), self.home.path(), &mut nodes);
        nodes.sort_by(|a, b| a.0.cmp(&b.0));
        nodes
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Node {
    Directory,
    Link(PathBuf),
    File(Vec<u8>),
}

pub fn put(path: &Path, content: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

pub fn output_json(result: &assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&result.get_output().stdout).unwrap()
}
