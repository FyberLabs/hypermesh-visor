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

/// Names and purpose. Long enough for a sentence, short enough that
/// `GET /companion` stays far under the companion's read cap.
pub const SESSION_TEXT_MAX: usize = 512;
/// Instructions and recipe steps.
pub const SESSION_BODY_MAX: usize = 2_048;
/// Recipes, skills, and steps in one recipe.
pub const SESSION_LIST_MAX: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub enum HarnessError {
    PurposeRequired,
    AgentNameRequired,
    RecipeNameRequired,
    SkillNameRequired,
    TooLong(&'static str),
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PurposeRequired => write!(f, "purpose is required"),
            Self::AgentNameRequired => write!(f, "agent name is required"),
            Self::RecipeNameRequired => write!(f, "recipe name is required"),
            Self::SkillNameRequired => write!(f, "skill name is required"),
            Self::TooLong(what) => write!(f, "{what} is too long"),
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
        limited(&self.purpose, SESSION_TEXT_MAX, "purpose")?;
        limited(&self.agent.name, SESSION_TEXT_MAX, "agent name")?;
        limited(
            &self.agent.instructions,
            SESSION_BODY_MAX,
            "agent instructions",
        )?;
        if self.recipes.len() > SESSION_LIST_MAX {
            return Err(HarnessError::TooLong("recipes"));
        }
        if self.skills.len() > SESSION_LIST_MAX {
            return Err(HarnessError::TooLong("skills"));
        }
        for recipe in &self.recipes {
            limited(&recipe.name, SESSION_TEXT_MAX, "recipe name")?;
            if recipe.steps.len() > SESSION_LIST_MAX {
                return Err(HarnessError::TooLong("recipe steps"));
            }
            for step in &recipe.steps {
                limited(step, SESSION_BODY_MAX, "recipe step")?;
            }
        }
        for skill in &self.skills {
            limited(&skill.name, SESSION_TEXT_MAX, "skill name")?;
            limited(&skill.instructions, SESSION_BODY_MAX, "skill instructions")?;
        }
        Ok(())
    }
}

fn limited(value: &str, max: usize, field: &'static str) -> Result<(), HarnessError> {
    if value.chars().count() > max {
        Err(HarnessError::TooLong(field))
    } else {
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
        let mut huge = harness();
        huge.purpose = "p".repeat(SESSION_TEXT_MAX + 1);
        assert_eq!(huge.validate(), Err(HarnessError::TooLong("purpose")));
        huge = harness();
        huge.recipes = (0..SESSION_LIST_MAX + 1)
            .map(|index| Recipe {
                name: format!("r{index}"),
                steps: Vec::new(),
            })
            .collect();
        assert_eq!(huge.validate(), Err(HarnessError::TooLong("recipes")));
    }
}
