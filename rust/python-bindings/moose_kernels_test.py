import numpy as np
from absl.testing import absltest
from absl.testing import parameterized
from moose_kernels import ring_add
from moose_kernels import ring_fill
from moose_kernels import ring_mul
from moose_kernels import ring_shape
from moose_kernels import ring_sub
from moose_kernels import sample_key


class BinaryOp(parameterized.TestCase):
    @parameterized.parameters(
        (lambda x, y: x + y, ring_add),
        (lambda x, y: x * y, ring_mul),
        (lambda x, y: x - y, ring_sub),
    )
    def test_usual_binary_op(self, numpy_lmbd, moose_op):
        a = np.array([1, 2, 3], dtype=np.uint64)
        b = np.array([4, 5, 6], dtype=np.uint64)

        c1 = numpy_lmbd(b, a)
        c2 = moose_op(b, a)

        np.testing.assert_array_equal(c1, c2)

    def test_shape(self):
        a = np.array([1, 2, 3], dtype=np.uint64)
        assert ring_shape(a) == [3]


class SamplingOperations(parameterized.TestCase):
    def test_sample_key(self):
        key = sample_key()
        assert len(key) == 16
        assert isinstance(key, bytes)


class FillOp(parameterized.TestCase):
    def test_fill_op(self):
        actual = ring_fill((2, 2), 1)
        expected = np.full((2, 2), 1, dtype=np.uint64)
        np.testing.assert_array_equal(actual, expected)


if __name__ == "__main__":
    absltest.main()
