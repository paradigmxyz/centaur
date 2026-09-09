use std::collections::BTreeSet;

use crate::tool_discovery::DiscoveredTool;

pub(super) fn search(tools: Vec<DiscoveredTool>, query: &str) -> Vec<DiscoveredTool> {
    let query = normalize(query);
    let terms = query.split_whitespace().collect::<BTreeSet<_>>();
    let mut matches = tools
        .into_iter()
        .filter_map(|tool| {
            let name = normalize(&tool.name);
            let description = normalize(tool.description.as_deref().unwrap_or_default());
            let name_terms = name.split_whitespace().collect::<BTreeSet<_>>();
            let all_terms = name_terms
                .iter()
                .copied()
                .chain(description.split_whitespace())
                .collect::<BTreeSet<_>>();
            let name_matches = terms.intersection(&name_terms).count();
            let coverage = terms.intersection(&all_terms).count();
            (coverage > 0).then_some(((name == query, name_matches, coverage), tool))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.name.cmp(&right.name))
    });
    matches.into_iter().map(|(_, tool)| tool).collect()
}

fn normalize(text: &str) -> String {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, description: Option<&str>) -> DiscoveredTool {
        DiscoveredTool {
            name: name.to_owned(),
            description: description.map(str::to_owned),
            package: name.to_owned(),
            client_module: format!("{name}.client"),
            project_dir: name.into(),
        }
    }

    fn names(tools: Vec<DiscoveredTool>, query: &str) -> Vec<String> {
        search(tools, query)
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    #[test]
    fn ranks_exact_names_then_name_tokens_then_description_coverage() {
        let tools = vec![
            tool("archive", Some("Search Slack messages")),
            tool("slack-export", Some("Export archives")),
            tool(
                "slack",
                Some("Search Slack messages, channels, threads, users"),
            ),
            tool("lookup", Some("Search documents")),
        ];
        assert_eq!(
            names(tools.clone(), " SLACK! "),
            ["slack", "slack-export", "archive"]
        );
        assert_eq!(
            names(tools.clone(), "search, slack messages!"),
            ["slack", "slack-export", "archive", "lookup"]
        );
        assert_eq!(
            names(tools, "search messages"),
            ["archive", "slack", "lookup"]
        );
    }

    #[test]
    fn matches_names_and_descriptions_and_handles_empty_or_unmatched_queries() {
        let tools = vec![
            tool("with", None),
            tool("messaging", Some("Send and create messages with users")),
        ];
        assert_eq!(names(tools.clone(), "with"), ["with", "messaging"]);
        assert_eq!(names(tools.clone(), "send"), ["messaging"]);
        assert_eq!(names(tools.clone(), "create messages"), ["messaging"]);
        for query in ["", "  ", "---", "unfindable"] {
            assert!(names(tools.clone(), query).is_empty(), "query: {query}");
        }
    }

    #[test]
    fn splits_identifier_separators_preserves_unicode_and_breaks_ties_by_name() {
        let tools = vec![
            tool("zeta", Some("Café issue tracking")),
            tool("issue-tracker", None),
            tool("issue_tracker", None),
            tool("alpha", Some("Café issue tracking")),
        ];
        assert_eq!(
            names(tools.clone(), "ISSUE_tracker"),
            ["issue-tracker", "issue_tracker", "alpha", "zeta"]
        );
        assert_eq!(names(tools.clone(), "CAFÉ"), ["alpha", "zeta"]);
        assert_eq!(names(tools.clone(), "issue issue"), names(tools, "issue"));
    }
}
