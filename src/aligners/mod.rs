use std::{borrow::Cow, sync::Arc};

use indicatif::ParallelProgressIterator;
use rayon::prelude::*;
use thread_local::ThreadLocal;

use crate::{
    haec_io::HAECRecord,
    overlaps::{self, Overlap, Strand},
};

pub mod wfa;

#[derive(Debug, PartialEq, Clone, Eq)]
pub enum CigarOp {
    Match(u32),
    Mismatch(u32),
    Insertion(u32),
    Deletion(u32),
}

impl CigarOp {
    pub fn reverse(&self) -> Self {
        match self {
            Self::Insertion(l) => Self::Deletion(*l),
            Self::Deletion(l) => Self::Insertion(*l),
            _ => self.clone(),
        }
    }

    pub fn get_length(&self) -> u32 {
        match self {
            Self::Match(l) => *l,
            Self::Mismatch(l) => *l,
            Self::Insertion(l) => *l,
            Self::Deletion(l) => *l,
        }
    }

    pub fn with_length(&self, length: u32) -> Self {
        match self {
            Self::Match(_) => CigarOp::Match(length),
            Self::Mismatch(_) => CigarOp::Mismatch(length),
            Self::Insertion(_) => CigarOp::Insertion(length),
            Self::Deletion(_) => CigarOp::Deletion(length),
        }
    }
}

impl From<(u32, char)> for CigarOp {
    fn from(cigar: (u32, char)) -> Self {
        match cigar.1 {
            'M' => CigarOp::Match(cigar.0),
            'X' => CigarOp::Mismatch(cigar.0),
            'I' => CigarOp::Insertion(cigar.0),
            'D' => CigarOp::Deletion(cigar.0),
            _ => panic!("Invalid cigar op {}", cigar.1),
        }
    }
}

impl ToString for CigarOp {
    fn to_string(&self) -> String {
        match self {
            CigarOp::Match(l) => format!("{}{}", l, '='),
            CigarOp::Mismatch(l) => format!("{}{}", l, 'X'),
            CigarOp::Deletion(l) => format!("{}{}", l, 'D'),
            CigarOp::Insertion(l) => format!("{}{}", l, 'I'),
        }
    }
}

pub fn cigar_to_string(cigar: &[CigarOp]) -> String {
    cigar.iter().map(|op| op.to_string()).collect()
}

#[inline]
fn complement(base: u8) -> u8 {
    match base {
        b'A' => b'T', // A -> T
        b'C' => b'G', // C -> G
        b'G' => b'C', // G -> C
        b'T' => b'A', // T -> A
        _ => panic!("Invalid base."),
    }
}

pub fn reverse_complement(seq: &[u8]) -> Vec<u8> {
    seq.iter().rev().map(|c| complement(*c)).collect()
}

pub fn align_overlaps(overlaps: &mut [Overlap], reads: &[HAECRecord]) {
    let n_overlaps = overlaps.len();
    let aligners = Arc::new(ThreadLocal::new());

    overlaps
        .par_iter_mut()
        //.with_min_len(10)
        .progress_count(n_overlaps as u64)
        .for_each_with(aligners, |aligners, o| {
            let aligner = aligners.get_or(|| wfa::WFAAligner::default());

            let query = &reads[o.qid as usize].seq[o.qstart as usize..o.qend as usize];
            let query = match o.strand {
                overlaps::Strand::Forward => Cow::Borrowed(query),
                overlaps::Strand::Reverse => Cow::Owned(reverse_complement(query)),
            };

            let target = &reads[o.tid as usize].seq[o.tstart as usize..o.tend as usize];

            let align_result = aligner.align(&query, target).unwrap();
            o.cigar = Some(align_result.cigar);

            o.tstart += align_result.tstart;
            o.tend -= align_result.tend;

            match o.strand {
                overlaps::Strand::Forward => {
                    o.qstart += align_result.qstart;
                    o.qend -= align_result.qend;
                }
                overlaps::Strand::Reverse => {
                    o.qstart += align_result.qend;
                    o.qend -= align_result.qstart;
                }
            }

            o.accuracy = Some(calculate_accuracy(o.cigar.as_ref().unwrap()));
        });
}

fn calculate_accuracy(cigar: &[CigarOp]) -> f32 {
    let (mut matches, mut subs, mut ins, mut dels) = (0u32, 0u32, 0u32, 0u32);
    for op in cigar {
        match op {
            CigarOp::Match(l) => matches += l,
            CigarOp::Mismatch(l) => subs += l,
            CigarOp::Insertion(l) => ins += l,
            CigarOp::Deletion(l) => dels += l,
        };
    }

    let length = (matches + subs + ins + dels) as f32;
    matches as f32 / length
}

pub struct AlignmentResult {
    cigar: Vec<CigarOp>,
    tstart: u32,
    tend: u32,
    qstart: u32,
    qend: u32,
}

impl AlignmentResult {
    fn new(cigar: Vec<CigarOp>, tstart: u32, tend: u32, qstart: u32, qend: u32) -> Self {
        AlignmentResult {
            cigar,
            tstart,
            tend,
            qstart,
            qend,
        }
    }
}

pub(crate) fn get_proper_cigar(cigar: &[CigarOp], is_target: bool, strand: Strand) -> Vec<CigarOp> {
    if is_target {
        return cigar.to_owned();
    }

    let iter = cigar.iter().map(move |c| c.reverse());
    if let Strand::Reverse = strand {
        return iter.rev().collect();
    }

    iter.collect()
}

pub(crate) fn fix_cigar(cigar: &mut Vec<CigarOp>, target: &[u8], query: &[u8]) -> (u32, u32) {
    // Left-alignment of indels
    // https://github.com/lh3/minimap2/blob/master/align.c#L91

    let (mut tpos, mut qpos) = (0usize, 0usize);
    for i in 0..cigar.len() {
        if let CigarOp::Match(l) | CigarOp::Mismatch(l) = &cigar[i] {
            tpos += *l as usize;
            qpos += *l as usize;
        } else {
            if i > 0
                && i < cigar.len() - 1
                && matches!(cigar[i - 1], CigarOp::Match(_) | CigarOp::Mismatch(_))
                && matches!(cigar[i + 1], CigarOp::Match(_) | CigarOp::Mismatch(_))
            {
                let prev_len = match &cigar[i - 1] {
                    CigarOp::Match(pl) => *pl as usize,
                    CigarOp::Mismatch(pl) => *pl as usize,
                    _ => unreachable!(),
                };
                let mut l = 0;

                if let CigarOp::Insertion(len) = &cigar[i] {
                    while l < prev_len {
                        if query[qpos - 1 - l] != query[qpos + *len as usize - 1 - l] {
                            break;
                        }

                        l += 1;
                    }
                } else {
                    let len = cigar[i].get_length() as usize;

                    while l < prev_len {
                        if target[tpos - 1 - l] != target[tpos + len - 1 - l] {
                            break;
                        }

                        l += 1;
                    }
                }

                if l > 0 {
                    cigar[i - 1] = match &cigar[i - 1] {
                        CigarOp::Match(v) => CigarOp::Match(*v - l as u32),
                        CigarOp::Mismatch(v) => CigarOp::Mismatch(*v - l as u32),
                        _ => unreachable!(),
                    };

                    cigar[i + 1] = match &cigar[i + 1] {
                        CigarOp::Match(v) => CigarOp::Match(*v + l as u32),
                        CigarOp::Mismatch(v) => CigarOp::Mismatch(*v + l as u32),
                        _ => unreachable!(),
                    };

                    tpos -= l;
                    qpos -= l;
                }

                match &cigar[i] {
                    CigarOp::Insertion(len) => qpos += *len as usize,
                    CigarOp::Deletion(len) => tpos += *len as usize,
                    _ => unreachable!(),
                }
            }
        }
    }

    let mut is_start = true;
    let (mut tshift, mut qshift) = (0, 0);
    cigar.retain(|op| {
        if is_start {
            match op {
                CigarOp::Match(l) | CigarOp::Mismatch(l) if *l > 0 => {
                    is_start = false;
                    return true;
                }
                CigarOp::Match(_) | CigarOp::Mismatch(_) => return false,
                CigarOp::Insertion(ref l) => {
                    is_start = false;
                    qshift = *l;
                    return false;
                }
                CigarOp::Deletion(ref l) => {
                    is_start = false;
                    tshift = *l;
                    return false;
                }
            }
        }

        if op.get_length() > 0 {
            return true;
        } else {
            return false;
        };
    });

    let mut l = 0;
    for i in 0..cigar.len() {
        if i == cigar.len() - 1
            || std::mem::discriminant(&cigar[i]) != std::mem::discriminant(&cigar[i + 1])
        {
            cigar[l] = cigar[i].clone();
            l += 1;
        } else {
            cigar[i + 1] = cigar[i].with_length(cigar[i].get_length() + cigar[i + 1].get_length());
        }
    }
    cigar.drain(l..);

    (tshift, qshift)
}

#[cfg(test)]
mod tests {
    use super::{fix_cigar, CigarOp};

    #[test]
    fn fix_cigar_test1() {
        let target = "TTTTGTTTTTTTTTTCTTTTTTTTTTTTTTTTTTTGCT".as_bytes();
        let query = "TTTTGTTTTTTTTTTCTTTTTTTTTTTTTTTGCT".as_bytes();
        let mut cigar = vec![CigarOp::Match(31), CigarOp::Deletion(4), CigarOp::Match(3)];

        fix_cigar(&mut cigar, target, query);
        assert_eq!(
            cigar,
            [CigarOp::Match(16), CigarOp::Deletion(4), CigarOp::Match(18)]
        )
    }

    #[test]
    fn fix_cigar_test2() {
        let target = "AGCAAAAAAAAAAAAAAAGAAAAAAAAAACAAAA".as_bytes();
        let query = "AGCAAAAAAAAAAAAAAAAAAAGAAAAAAAAAACAAAA".as_bytes();
        let mut cigar = vec![
            CigarOp::Match(18),
            CigarOp::Insertion(4),
            CigarOp::Match(16),
        ];

        fix_cigar(&mut cigar, target, query);
        assert_eq!(
            cigar,
            [CigarOp::Match(3), CigarOp::Insertion(4), CigarOp::Match(31)]
        )
    }

    #[test]
    fn fix_cigar_test3() {
        let target = "CACCAGGCCA".as_bytes();
        let query = "CACCAGCCA".as_bytes();
        let mut cigar = vec![CigarOp::Match(6), CigarOp::Deletion(1), CigarOp::Match(3)];

        fix_cigar(&mut cigar, target, query);
        assert_eq!(
            cigar,
            [CigarOp::Match(5), CigarOp::Deletion(1), CigarOp::Match(4)]
        )
    }
}
