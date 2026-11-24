//! Mutation fuzzing of every decoder: valid datagrams with bytes flipped,
//! cut, repeated and shuffled, and plain random bytes, must decode to
//! something or nothing, never panic or loop. A small fixed-seed generator
//! keeps runs repeatable without a dependency.

use std::net::IpAddr;

use crate::{decode, Templates};

/// xorshift64*: enough to spread mutations, the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

fn seeds() -> Vec<Vec<u8>> {
    use crate::template::tests::{ipfix, record, set, template, v9, FIELDS};
    let rec = record([192, 0, 2, 1], [198, 51, 100, 2], 443, 1000, 2);
    let frame = crate::packet::tests::frame([192, 0, 2, 1], [203, 0, 113, 1], 17, 1000, 53);
    vec![
        crate::netflow5::tests::datagram(&[([192, 0, 2, 1], [192, 0, 2, 2], 6, 1, 2, 3, 4); 3], 7),
        v9(&[set(0, &template(300, &FIELDS)), set(300, &rec)]),
        ipfix(&[set(2, &template(300, &FIELDS)), set(300, &rec)]),
        // IPFIX with a variable-length field.
        ipfix(&[
            set(2, &template(301, &[(8, 4), (12, 4), (1, 0xffff)])),
            set(301, &[192, 0, 2, 1, 192, 0, 2, 2, 255, 0, 3, 1, 2, 3]),
        ]),
        crate::sflow::tests::datagram(
            [192, 0, 2, 9],
            &[crate::sflow::tests::flow_sample(100, 1500, &frame)],
        ),
    ]
}

fn mutate(rng: &mut Rng, data: &mut Vec<u8>) {
    for _ in 0..=rng.below(4) {
        match rng.below(6) {
            0 if !data.is_empty() => {
                let i = rng.below(data.len());
                data[i] = rng.next() as u8;
            }
            1 if !data.is_empty() => {
                let i = rng.below(data.len());
                data[i] = [0, 0xff, 0x7f, 0x80, 1][rng.below(5)];
            }
            2 => data.truncate(rng.below(data.len() + 1)),
            3 if !data.is_empty() => {
                let from = rng.below(data.len());
                let to = (from + rng.below(64)).min(data.len());
                let copy = data[from..to].to_vec();
                let at = rng.below(data.len());
                data.splice(at..at, copy);
            }
            4 => {
                let at = rng.below(data.len() + 1);
                data.splice(at..at, (0..rng.below(16)).map(|_| rng.next() as u8));
            }
            _ if data.len() >= 2 => {
                // A length or count at its extremes.
                let i = rng.below(data.len() - 1);
                let v: u16 = [0, 1, 0xffff, 0x8000, 4][rng.below(5)];
                data[i..i + 2].copy_from_slice(&v.to_be_bytes());
            }
            _ => {}
        }
    }
}

#[test]
fn mutated_datagrams_never_panic() {
    let exporter: IpAddr = "192.0.2.254".parse().unwrap();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    // One template cache across runs, so mutated templates meet mutated
    // data as with a real exporter.
    let mut templates = Templates::default();
    let seeds = seeds();
    for seed in &seeds {
        assert!(
            !decode(seed, exporter, &mut templates).malformed,
            "the seed is valid"
        );
    }
    for _ in 0..200_000 {
        let mut data = seeds[rng.below(seeds.len())].clone();
        mutate(&mut rng, &mut data);
        let out = decode(&data, exporter, &mut templates);
        assert!(out.flows.len() <= data.len(), "never more flows than bytes");
    }
}

#[test]
fn random_bytes_never_panic() {
    let exporter: IpAddr = "2001:db8::1".parse().unwrap();
    let mut rng = Rng(42);
    let mut templates = Templates::default();
    for _ in 0..100_000 {
        let len = rng.below(600);
        let mut data: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        // Half of them look like one of the formats.
        if data.len() >= 4 && rng.below(2) == 0 {
            let head: [&[u8]; 4] = [&[0, 5], &[0, 9], &[0, 10], &[0, 0, 0, 5]];
            let h = head[rng.below(4)];
            data[..h.len()].copy_from_slice(h);
        }
        decode(&data, exporter, &mut templates);
    }
}
