use pretty_assertions::assert_eq;

fn main() {
    let s = String::from("test");

    let reversed = s.chars().rev().collect::<String>();

    assert_eq!(reversed, "tset");
}
