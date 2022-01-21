use super::*;
use crate::computation::MaximumOp;
use crate::error::Result;
use macros::with_context;

impl MaximumOp {
    pub(crate) fn kernel<S: Session, RepRingT, RepBitT, MirRingT>(
        sess: &S,
        plc: &ReplicatedPlacement,
        x: &[RepRingT],
    ) -> Result<RepRingT>
    where
        RepRingT: Clone,
        ReplicatedPlacement: PlacementLessThan<S, RepRingT, RepRingT, RepBitT>,
        ReplicatedPlacement: PlacementMul<S, RepRingT, RepRingT, RepRingT>,
        ReplicatedPlacement: PlacementRingInject<S, RepBitT, RepRingT>,
        ReplicatedPlacement: PlacementNeg<S, RepRingT, RepRingT>,
        ReplicatedPlacement: PlacementMaximum<S, RepRingT, RepRingT>,
        ReplicatedPlacement: PlacementAdd<S, RepRingT, RepRingT, RepRingT>,
        ReplicatedPlacement: ShapeFill<S, RepRingT, Result = MirRingT>,
        ReplicatedPlacement: PlacementSub<S, MirRingT, RepRingT, RepRingT>,
    {
        let n = x.len();
        if n == 0 {
            Err(Error::InvalidArgument(
                "maximum op needs a non-empty array of tensors".to_string(),
            ))
        } else if n == 1 {
            Ok(x[0].clone())
        } else {
            let chunk1 = &x[0..n / 2];
            let chunk2 = &x[n / 2..n];
            let max_chunk1 = plc.maximum(sess, chunk1);
            let max_chunk2 = plc.maximum(sess, chunk2);

            let lesser = plc.less(sess, &max_chunk1, &max_chunk2);

            let lesser_ring = plc.ring_inject(sess, 0, &lesser);
            let ones = plc.shape_fill(sess, Constant::Ring64(1), &lesser_ring);

            let expr = with_context!(
                plc,
                sess,
                lesser_ring * max_chunk2 + (ones - lesser_ring) * max_chunk1
            );
            Ok(expr)
        }
    }
}

impl SoftmaxOp {
    pub(crate) fn rep_fixed_kernel<S: Session, RepFixedT, ShapeT, RepRingT, RepBitT>(
        sess: &S,
        rep: &ReplicatedPlacement,
        axis: Option<u32>,
        upmost_index: usize,
        x: RepFixedT,
    ) -> Result<RepFixedT>
    where
        RepFixedT: FixedpointTensor,
        ReplicatedPlacement: PlacementIndexAxis<S, RepFixedT, RepFixedT>,
        ReplicatedPlacement: PlacementSub<S, RepFixedT, RepFixedT, RepFixedT>,
        ReplicatedPlacement: PlacementMaximum<S, RepFixedT, RepFixedT>,
        RepFixedTensor<RepRingT>: Into<RepFixedT>,
        ReplicatedPlacement: PlacementFill<S, ShapeT, RepRingT>,
        ReplicatedPlacement: PlacementShape<S, RepFixedT, ShapeT>,
        ReplicatedPlacement: PlacementSub<S, RepFixedT, RepFixedT, RepFixedT>,
        ReplicatedPlacement: PlacementGreaterThan<S, RepFixedT, RepFixedT, RepBitT>,
        ReplicatedPlacement: PlacementRingInject<S, RepBitT, RepRingT>,
        ReplicatedPlacement: PlacementMux<S, RepRingT, RepFixedT, RepFixedT, RepFixedT>,
        ReplicatedPlacement: PlacementExp<S, RepFixedT, RepFixedT>,
        ReplicatedPlacement: PlacementDiv<S, RepFixedT, RepFixedT, RepFixedT>,
        ReplicatedPlacement: PlacementSum<S, RepFixedT, RepFixedT>,
    {
        let xs: Vec<_> = (0..upmost_index)
            .map(|index| rep.index_axis(sess, axis.map(|a| a as usize).unwrap(), index, &x))
            .collect();

        let xmax = rep.maximum(sess, &xs);
        let max_diff = rep.sub(sess, &x, &xmax);

        let e_x = rep.exp(sess, &max_diff);

        // input sanitization
        let zeros_fill = rep.fill(sess, 0_u8.into(), &rep.shape(sess, &e_x));
        let zeros_rep = RepFixedTensor {
            tensor: zeros_fill,
            integral_precision: x.integral_precision(),
            fractional_precision: x.fractional_precision(),
        }
        .into();

        let min_val = (-1_f64 * 2_f64.powf((x.integral_precision() - 1).into()).ln())
            .as_fixedpoint(x.fractional_precision() as usize);

        let min_val_fill = rep.fill(sess, min_val.into(), &rep.shape(sess, &max_diff));
        let lower_bound = RepFixedTensor {
            tensor: min_val_fill,
            integral_precision: x.integral_precision(),
            fractional_precision: x.fractional_precision(),
        }
        .into();

        // x - max(x) > get_limit
        let threshold = rep.greater_than(sess, &lower_bound, &max_diff);
        let threshold_ring = rep.ring_inject(sess, 0, &threshold);
        let normalized = rep.mux(sess, &threshold_ring, &zeros_rep, &e_x);

        let e_x_sum = rep.sum(sess, axis, &normalized);
        let softmax = rep.div(sess, &normalized, &e_x_sum);

        Ok(softmax)
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::fixedpoint::{Convert, FixedTensor};
    use crate::host::HostRingTensor;
    use crate::kernels::SyncSession;
    use ndarray::prelude::*;
    use ndarray::Zip;

    fn new_host_fixed_tensor_with_precision<HostRingT>(
        x: HostRingT,
        integral_precision: u32,
        fractional_precision: u32,
    ) -> HostFixedTensor<HostRingT> {
        HostFixedTensor {
            tensor: x,
            integral_precision,
            fractional_precision,
        }
    }

    macro_rules! rep_approx_softmax_fixed_test {
        ($func_name:ident, $test_func: ident<$ti: ty, $tu: ty>, $axis: expr, $upmost_index: expr, $i_precision: expr, $f_precision: expr, $err: expr) => {
            fn $func_name(x: ArrayD<f64>, y_target: Vec<f64>) {
                let alice = HostPlacement {
                    owner: "alice".into(),
                };
                let rep = ReplicatedPlacement {
                    owners: ["alice".into(), "bob".into(), "carole".into()],
                };

                let sess = SyncSession::default();
                let encode = |item: &f64| -> $tu {
                    let tmp: $ti = (2f64.powf($f_precision as f64) * item) as $ti;
                    tmp as $tu
                };
                let x_encoded = x.map(encode);

                let x = FixedTensor::Host(new_host_fixed_tensor_with_precision(
                    HostRingTensor::from_raw_plc(x_encoded.clone(), alice.clone()), $i_precision, $f_precision)
                );

                let exp_result = rep.$test_func(&sess, Some($axis), $upmost_index, &x);

                let opened_exp = match exp_result {
                    FixedTensor::Replicated(r) => alice.reveal(&sess, &r),
                    _ => panic!("Should not produce an non-replicated tensor on a replicated placement"),
                };

                let result = Convert::decode(&opened_exp.tensor, (2 as $tu).pow($f_precision));
                // operation precision is not as accurate as the fixed point precision
                for i in 0..y_target.len() {
                    let error = (result.0[i] - y_target[i]).abs();
                    assert!(error < $err, "failed at index {:?}, error is {:?}", i, error);
                }
            }
        };
    }

    rep_approx_softmax_fixed_test!(test_rep_softmax_fixed64, softmax<i64, u64>, 0, 9, 8, 10, 0.01);
    rep_approx_softmax_fixed_test!(test_rep_softmax_fixed128, softmax<i128, u128>, 0, 9, 8, 27, 0.01);

    #[test]
    fn test_softmax_64() {
        let x = array![1f64, 2.5, -3.0, 4.0, 2.0, -2.0, -2.0, -3.0, 3.0].into_dyn();
        let mut x_max = x.index_axis(Axis(0), 0).to_owned();

        for x_item in x.axis_iter(Axis(0)) {
            Zip::from(&mut x_max)
                .and(&x_item)
                .for_each(|entry_a, &entry_b| *entry_a = f64::max(*entry_a, entry_b));
        }
        let y = x.clone() - x_max;
        let y_exp = y.map(|item| item.exp());
        let softmax = y_exp.clone() / y_exp.sum_axis(Axis(0));

        let y_targets: Vec<_> = softmax.iter().map(|item| item.clone()).collect();

        test_rep_softmax_fixed64(x, y_targets);
    }

    #[test]
    fn test_softmax_128() {
        let x = array![1f64, 2.5, -3.0, 4.0, 2.0, -2.0, -2.0, -3.0, 3.0].into_dyn();
        let mut x_max = x.index_axis(Axis(0), 0).to_owned();

        for x_item in x.axis_iter(Axis(0)) {
            Zip::from(&mut x_max)
                .and(&x_item)
                .for_each(|entry_a, &entry_b| *entry_a = f64::max(*entry_a, entry_b));
        }
        let y = x.clone() - x_max;
        let y_exp = y.map(|item| item.exp());
        let softmax = y_exp.clone() / y_exp.sum_axis(Axis(0));

        let y_targets: Vec<_> = softmax.iter().map(|item| item.clone()).collect();

        test_rep_softmax_fixed128(x, y_targets);
    }
}
