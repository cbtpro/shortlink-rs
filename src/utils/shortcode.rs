use rand::distr::Alphanumeric;
use rand::Rng;

pub fn generate_random_string(len: usize) -> String {
    let mut rng = rand::rng();
    (0..len)
        .map(|_| rng.sample(Alphanumeric))
        .map(char::from)
        .collect()
}