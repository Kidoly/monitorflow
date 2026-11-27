//! Name suggestions for matching customers to PSA clients (ADR-0164,
//! ADR-0191): names compared without case, accents, punctuation or the
//! usual company suffixes. A suggestion is only that: nothing is matched
//! until an administrator confirms it.

use std::collections::HashMap;

use super::halopsa::Record;

/// Company forms left out of a name before comparing, as whole words.
const SUFFIXES: &[&str] = &[
    "ag",
    "bv",
    "co",
    "company",
    "corp",
    "corporation",
    "eurl",
    "gmbh",
    "group",
    "groupe",
    "inc",
    "incorporated",
    "limited",
    "llc",
    "llp",
    "ltd",
    "nv",
    "plc",
    "pty",
    "sa",
    "sarl",
    "sas",
    "sasu",
    "scop",
    "sl",
    "spa",
    "srl",
];

/// `name` folded for comparing: lowercase, without accents, with
/// punctuation as spaces and without company suffixes. "Exemple SAS." and
/// "exemple" fold the same.
pub fn normalized(name: &str) -> String {
    let folded: String = name
        .chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'æ' => 'a',
            'ç' => 'c',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ñ' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
            'œ' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ý' | 'ÿ' => 'y',
            '&' => ' ',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        })
        .collect();
    let words: Vec<&str> = folded.split_whitespace().collect();
    let kept: Vec<&str> = words
        .iter()
        .copied()
        .filter(|w| !SUFFIXES.contains(w))
        .collect();
    // A name made only of suffixes keeps them, so it still compares.
    if kept.is_empty() { words } else { kept }.join(" ")
}

/// For each of `customers` (id, name) without a link, the client of
/// `clients` whose name folds the same, when exactly one does and no other
/// customer folds to it too. `taken` are clients matched already.
pub fn suggest<'a, Id: Copy + Eq + std::hash::Hash>(
    customers: &'a [(Id, String)],
    clients: &'a [Record],
    taken: &[i64],
) -> Vec<(Id, &'a Record)> {
    let mut by_name: HashMap<String, Vec<&Record>> = HashMap::new();
    for client in clients.iter().filter(|c| !taken.contains(&c.id)) {
        by_name
            .entry(normalized(&client.name))
            .or_default()
            .push(client);
    }
    let mut customers_by_name: HashMap<String, usize> = HashMap::new();
    for (_, name) in customers {
        *customers_by_name.entry(normalized(name)).or_default() += 1;
    }
    customers
        .iter()
        .filter_map(|(id, name)| {
            let key = normalized(name);
            if key.is_empty() || customers_by_name.get(&key) != Some(&1) {
                return None;
            }
            match by_name.get(&key).map(Vec::as_slice) {
                Some([client]) => Some((*id, *client)),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(id: i64, name: &str) -> Record {
        Record {
            id,
            name: name.into(),
        }
    }

    #[test]
    fn names_fold_without_case_accents_punctuation_or_suffixes() {
        assert_eq!(normalized("Exemple Industries SAS."), "exemple industries");
        assert_eq!(normalized("EXEMPLE-INDUSTRIES"), "exemple industries");
        assert_eq!(
            normalized("Écoles Réunies, S.A.R.L"),
            "ecoles reunies s a r l"
        );
        assert_eq!(normalized("Écoles Réunies SARL"), "ecoles reunies");
        assert_eq!(normalized("Sample Corp, Inc."), "sample");
        assert_eq!(normalized("Smith & Sons Ltd"), "smith sons");
        assert_eq!(normalized("SAS"), "sas");
        assert_eq!(normalized("  "), "");
    }

    #[test]
    fn only_unambiguous_names_are_suggested() {
        let clients = [
            client(10, "Example Industries SAS"),
            client(11, "Example Logistics"),
            client(12, "Twin"),
            client(13, "Twin Ltd"),
            client(14, "Taken Corp"),
        ];
        let customers = vec![
            (1, "example industries".to_owned()),
            (2, "Twin".to_owned()),
            (3, "Nothing Alike".to_owned()),
            (4, "Taken".to_owned()),
            (5, "Example Logistics".to_owned()),
            (6, "Example Logistics Inc".to_owned()),
        ];
        let suggested = suggest(&customers, &clients, &[14]);
        let ids: Vec<(i32, i64)> = suggested.iter().map(|(c, r)| (*c, r.id)).collect();
        // Two clients fold to "twin", two customers to "example logistics".
        assert_eq!(ids, [(1, 10)]);
    }
}
