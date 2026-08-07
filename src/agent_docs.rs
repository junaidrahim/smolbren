/// Canonical agent instructions shipped with the binary. Keeping the source
/// skill in the repository means `smolbren docs --agent` and installed skills
/// can be compared byte-for-byte in CI instead of drifting independently.
pub const SKILL: &str = include_str!("../skills/smolbren/SKILL.md");
