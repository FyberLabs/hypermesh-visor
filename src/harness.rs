use serde::{Deserialize, Serialize};

/// Passed in with the session. The image does not supply these.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Harness {
    pub purpose: String,
    pub recipes: Vec<Recipe>,
    pub agent: Agent,
    pub skills: Vec<Skill>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipe {
    pub name: String,
    #[serde(default)]
    pub steps: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    #[serde(default)]
    pub instructions: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    #[serde(default)]
    pub instructions: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HarnessError {
    PurposeRequired,
    AgentNameRequired,
    RecipeNameRequired,
    SkillNameRequired,
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PurposeRequired => write!(f, "purpose is required"),
            Self::AgentNameRequired => write!(f, "agent name is required"),
            Self::RecipeNameRequired => write!(f, "recipe name is required"),
            Self::SkillNameRequired => write!(f, "skill name is required"),
        }
    }
}

impl std::error::Error for HarnessError {}

impl Harness {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.purpose.trim().is_empty() {
            return Err(HarnessError::PurposeRequired);
        }
        if self.agent.name.trim().is_empty() {
            return Err(HarnessError::AgentNameRequired);
        }
        if self
            .recipes
            .iter()
            .any(|recipe| recipe.name.trim().is_empty())
        {
            return Err(HarnessError::RecipeNameRequired);
        }
        if self.skills.iter().any(|skill| skill.name.trim().is_empty()) {
            return Err(HarnessError::SkillNameRequired);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harness() -> Harness {
        Harness {
            purpose: "drive the desktop".into(),
            recipes: vec![Recipe {
                name: "focus".into(),
                steps: vec!["look".into()],
            }],
            agent: Agent {
                name: "desk".into(),
                instructions: "share the seat".into(),
            },
            skills: vec![Skill {
                name: "typing".into(),
                instructions: String::new(),
            }],
        }
    }

    #[test]
    fn accepts_a_passed_harness() {
        assert!(harness().validate().is_ok());
    }

    #[test]
    fn rejects_a_blank_harness() {
        let mut blank = harness();
        blank.purpose = "  ".into();
        assert_eq!(blank.validate(), Err(HarnessError::PurposeRequired));
        blank = harness();
        blank.agent.name.clear();
        assert_eq!(blank.validate(), Err(HarnessError::AgentNameRequired));
    }
}
