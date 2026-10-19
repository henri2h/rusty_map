//! Street names as IGN's BD TOPO stores them — upper case, the street type abbreviated
//! (`"QU VALERY GISCARD D'ESTAING"`) — turned into how they're written on signs
//! ("Quai Valery Giscard d'Estaing"). BD TOPO names carry no accents, so none can come back.

/// BD TOPO's street-type codes (the first word), spelled out.
const TYPES: &[(&str, &str)] = &[
    ("ALL", "Allée"),
    ("AV", "Avenue"),
    ("BD", "Boulevard"),
    ("CAR", "Carrefour"),
    ("CHE", "Chemin"),
    ("CHS", "Chaussée"),
    ("CITE", "Cité"),
    ("CRS", "Cours"),
    ("ESP", "Esplanade"),
    ("FG", "Faubourg"),
    ("GAL", "Galerie"),
    ("HAM", "Hameau"),
    ("IMP", "Impasse"),
    ("LOT", "Lotissement"),
    ("PARV", "Parvis"),
    ("PAS", "Passage"),
    ("PL", "Place"),
    ("PLE", "Passerelle"),
    ("PROM", "Promenade"),
    ("PRV", "Parvis"),
    ("PTE", "Porte"),
    ("QU", "Quai"),
    ("R", "Rue"),
    ("RES", "Résidence"),
    ("RLE", "Ruelle"),
    ("RPT", "Rond-point"),
    ("RTE", "Route"),
    ("SEN", "Sentier"),
    ("SQ", "Square"),
    ("TSSE", "Terrasse"),
    ("VLA", "Villa"),
    ("VOIE", "Voie"),
];

/// Small words written in lower case inside a name.
const PARTICLES: &[&str] = &[
    "à", "a", "au", "aux", "de", "des", "du", "en", "et", "la", "le", "les", "sous", "sur",
];

/// IGN's placeholder for "no name".
const NO_NAME: &str = "Valeur non renseignée";

/// The readable name, or `None` for an empty one or IGN's "no name" placeholder.
pub fn pretty_street(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case(NO_NAME) {
        return None;
    }
    // Already in mixed case (another source): leave it alone.
    if raw.chars().any(|c| c.is_lowercase()) {
        return Some(raw.to_string());
    }
    let mut words = raw.split_whitespace();
    let first = words.next()?;
    let mut out: Vec<String> = Vec::new();
    match TYPES.iter().find(|(code, _)| *code == first) {
        Some((_, name)) => out.push(name.to_string()),
        None => out.push(capitalise_word(first, true)),
    }
    out.extend(words.map(|word| capitalise_word(word, false)));
    Some(out.join(" "))
}

/// `"SAINT-GERMAIN"` → `"Saint-Germain"`, `"D'HONNEUR"` → `"d'Honneur"`, `"DE"` → `"de"`
/// (unless `first`).
fn capitalise_word(word: &str, first: bool) -> String {
    let lower = word.to_lowercase();
    if !first && PARTICLES.contains(&lower.as_str()) {
        return lower;
    }
    // Elided articles: `d'`, `l'` stay lower case, the word after them doesn't.
    for elided in ["d'", "l'"] {
        if let Some(rest) = lower.strip_prefix(elided)
            && !rest.is_empty()
        {
            let head = if first {
                elided.to_uppercase()
            } else {
                elided.to_string()
            };
            return format!("{head}{}", capitalise_word(rest, true));
        }
    }
    lower
        .split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<String>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spells_out_and_capitalises() {
        let cases = [
            ("ALL JEAN PAULHAN", "Allée Jean Paulhan"),
            ("BD SAINT-GERMAIN", "Boulevard Saint-Germain"),
            ("R DE LA LEGION D'HONNEUR", "Rue de la Legion d'Honneur"),
            (
                "QU VALERY GISCARD D'ESTAING",
                "Quai Valery Giscard d'Estaing",
            ),
            (
                "PL DU PRESIDENT EDOUARD HERRIOT",
                "Place du President Edouard Herriot",
            ),
            ("PONT ROYAL", "Pont Royal"),
            ("AV DES CHAMPS ELYSEES", "Avenue des Champs Elysees"),
        ];
        for (raw, expected) in cases {
            assert_eq!(pretty_street(raw).as_deref(), Some(expected), "{raw}");
        }
    }

    #[test]
    fn no_name_and_mixed_case() {
        assert_eq!(pretty_street(""), None);
        assert_eq!(pretty_street("Valeur non renseignée"), None);
        assert_eq!(
            pretty_street("Rue de Rivoli").as_deref(),
            Some("Rue de Rivoli")
        );
    }
}
