use serde_yaml_ng::{Mapping, Value};

/// Deep-merge two YAML values, right-biased (D14 semantics):
/// - mappings merge per-key recursively;
/// - sequences **append** (left then right) — no merge-by-key magic;
/// - any other shape: the right value replaces the left.
pub(crate) fn merge_values(base: Value, over: Value) -> Value {
    match (base, over) {
        (Value::Mapping(base), Value::Mapping(over)) => {
            let mut result = Mapping::new();
            for (k, v) in base {
                result.insert(k, v);
            }
            for (k, ov) in over {
                let merged = match result.remove(&k) {
                    Some(bv) => merge_values(bv, ov),
                    None => ov,
                };
                result.insert(k, merged);
            }
            Value::Mapping(result)
        }
        (Value::Sequence(mut base), Value::Sequence(over)) => {
            base.extend(over);
            Value::Sequence(base)
        }
        (_, over) => over,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(s: &str) -> Value {
        serde_yaml_ng::from_str(s).unwrap()
    }

    #[test]
    fn scalars_right_wins_maps_recurse() {
        let base = yaml("a: 1\nnested:\n  x: 1\n  y: 2\n");
        let over = yaml("a: 2\nnested:\n  y: 9\n  z: 3\n");
        let merged = merge_values(base, over);
        assert_eq!(merged, yaml("a: 2\nnested:\n  x: 1\n  y: 9\n  z: 3\n"));
    }

    #[test]
    fn sequences_append() {
        let base = yaml("list: [a, b]\n");
        let over = yaml("list: [c]\n");
        let merged = merge_values(base, over);
        assert_eq!(merged, yaml("list: [a, b, c]\n"));
    }
}
