import unittest

import numpy as np
from prepare_scene import frame_quad, unit_to_world


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.world = unit_to_world([[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0]])

    def test_outside_raster_bounds_are_clipped_even_when_refused(self):
        ti = [[1, 0, -100], [0, 1, 0], [0, 0, 1]]
        result = frame_quad(ti, self.world, 0, 100, 100)
        self.assertFalse(result["valid"])
        self.assertTrue(all(0 <= value <= 100 for value in result["bounds"]))

    def test_inverse_mapping_and_missing_or_crossing_horizon_pose(self):
        ti = [[1, 0, 0], [0, 1, 0], [0.2, 0.1, 1]]
        result = frame_quad(ti, self.world, 0, 100, 100)
        inverse = np.array(result["inverse"]).reshape(3, 3)
        uv = inverse @ [50 * 0.5 / 1.15 + 50, 50 * 0.5 / 1.15 + 50, 1]
        np.testing.assert_allclose(uv[:2] / uv[2], [0.5, 0.5], atol=1e-12)
        self.assertFalse(frame_quad(None, self.world, 0, 100, 100)["valid"])
        crossing = [[1, 0, 0], [0, 1, 0], [2, 0, -1]]
        self.assertFalse(frame_quad(crossing, self.world, 0, 100, 100)["valid"])


if __name__ == "__main__":
    unittest.main()
