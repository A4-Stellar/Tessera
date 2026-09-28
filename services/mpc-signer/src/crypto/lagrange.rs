use curve25519_dalek::scalar::Scalar;
use crate::types::NodeId;

/// Computes the Lagrange basis coefficient lambda_i(S) for participant i in subset S evaluated at 0:
/// lambda_i(0) = \prod_{j \in S, j \neq i} ( -j / (i - j) ) mod l
pub fn compute_lagrange_coefficient(i: NodeId, subset: &[NodeId]) -> Scalar {
    let mut num = Scalar::ONE;
    let mut den = Scalar::ONE;

    let i_scalar = Scalar::from(i as u64);

    for &j in subset {
        if j == i {
            continue;
        }
        let j_scalar = Scalar::from(j as u64);
        // numerator *= -j
        num *= -j_scalar;
        // denominator *= (i - j)
        den *= i_scalar - j_scalar;
    }

    // lambda_i = num * den^{-1} mod l
    num * den.invert()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lagrange_2_of_3_reconstruction() {
        // In 2-of-3: f(x) = a0 + a1 * x
        let a0 = Scalar::from(1337u64);
        let a1 = Scalar::from(42u64);

        let s1 = a0 + a1 * Scalar::from(1u64);
        let s2 = a0 + a1 * Scalar::from(2u64);
        let s3 = a0 + a1 * Scalar::from(3u64);

        // Test {1, 2}
        let l1 = compute_lagrange_coefficient(1, &[1, 2]);
        let l2 = compute_lagrange_coefficient(2, &[1, 2]);
        assert_eq!(l1 * s1 + l2 * s2, a0);

        // Test {2, 3}
        let l2_b = compute_lagrange_coefficient(2, &[2, 3]);
        let l3_b = compute_lagrange_coefficient(3, &[2, 3]);
        assert_eq!(l2_b * s2 + l3_b * s3, a0);

        // Test {1, 3}
        let l1_c = compute_lagrange_coefficient(1, &[1, 3]);
        let l3_c = compute_lagrange_coefficient(3, &[1, 3]);
        assert_eq!(l1_c * s1 + l3_c * s3, a0);
    }
}
