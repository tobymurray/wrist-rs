#!/usr/bin/env python3
"""Host tests for ble-probe-host.py: what reaches the fixture, and the bond step's identity."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('probe_host', Path(__file__).with_name('ble-probe-host.py'))
host = importlib.util.module_from_spec(spec)
spec.loader.exec_module(host)

LINE = 'probe matrix cell=17 order=link-first adv_ms=495 link_ms=250 update=0x00 answer=accepted done=0x00'


class Fixture(unittest.TestCase):
    def test_a_cell_the_link_went_in_is_left_for_its_resume(self):
        self.assertEqual(host.kept(f'[ble] {LINE} link=up'), f'{LINE} link=up')
        self.assertIsNone(host.kept(f'[ble] {LINE} link=lost'))
        self.assertIsNone(host.kept('[ble] probe matrix: the link went in cell 17; connect again'))

    def test_only_a_whole_run_replaces_the_fixture(self):
        whole = ['cell'] * 60
        self.assertEqual(host.destination('f.txt', 0, whole), ('f.txt', True))
        self.assertEqual(host.destination('f.txt', 0, whole[:59]), ('f.txt.partial', False))
        self.assertEqual(host.destination('f.txt', 0, []), ('f.txt.partial', False))
        self.assertEqual(host.destination('f.txt', 30, whole), ('f.txt.partial', False))


class Steps(unittest.TestCase):
    def test_every_scenario_parses(self):
        for name, steps in host.SCENARIOS.items():
            with self.subTest(name):
                self.assertTrue(host.parse_steps(steps))

    def test_a_step_takes_its_argument_and_options(self):
        self.assertEqual(host.parse_steps('clear; fill 0x21 hold=10 subscribe; reconnect last; forget test; wait 1.5'), [
            ('clear', None, {}),
            ('fill', 0x21, {'hold': '10', 'subscribe': 'yes'}),
            ('reconnect', 'last', {}),
            ('forget', 'test', {}),
            ('wait', 1.5, {}),
        ])

    def test_a_bad_step_is_refused_whole(self):
        for bad in ('bond', 'bond 0', 'bond last', 'fill 1 colour=red', 'fill 1 dist=all', 'bond 1 hangup=never', 'jump 3'):
            with self.subTest(bad), self.assertRaises(ValueError):
                host.parse_steps(f'clear; {bad}')

    def test_a_listing_and_what_it_lost(self):
        now = host.listed('[ble] bonded devices: 2 AC:12:03:D7:FC:5C C0:B0:4D:00:00:0A (random)')
        self.assertEqual(now, ['AC:12:03:D7:FC:5C', host.test_bond(10)])
        self.assertEqual(host.listed('[ble] bonded devices: 0'), [])
        self.assertIsNone(host.listed('[ble] bonded'))
        self.assertEqual(host.losses([host.test_bond(1), host.test_bond(10)], now), ([host.test_bond(1)], ['AC:12:03:D7:FC:5C']))

    def test_a_log_is_never_overwritten(self):
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            first = host.fresh(Path(d) / 'fill.log')
            self.assertEqual(first.name, 'fill.log')
            first.write_text('')
            second = host.fresh(Path(d) / 'fill.log')
            self.assertEqual(second.name, 'fill-2.log')
            second.write_text('')
            self.assertEqual(host.fresh(Path(d) / 'fill.log').name, 'fill-3.log')


@unittest.skipUnless(importlib.util.find_spec('bumble'), 'bumble is not installed')
class Bonds(unittest.TestCase):
    def test_each_pairing_gives_its_throwaway_address_as_its_identity(self):
        from bumble.pairing import PairingConfig, PairingDelegate
        config = host.pairing_config(PairingDelegate())
        self.assertEqual(config.identity_address_type, PairingConfig.AddressType.RANDOM)
        self.assertTrue(config.bonding and config.mitm and config.sc)

    def test_the_key_distributions(self):
        from bumble.pairing import PairingDelegate
        k = PairingDelegate.KeyDistribution
        self.assertEqual(host.key_distribution('default'), k.DISTRIBUTE_ENCRYPTION_KEY | k.DISTRIBUTE_IDENTITY_KEY)
        self.assertEqual(host.key_distribution('none'), 0)
        self.assertNotIn(k.DISTRIBUTE_IDENTITY_KEY, host.key_distribution('noid'))
        self.assertIn(k.DISTRIBUTE_SIGNING_KEY, host.key_distribution('sign'))


if __name__ == '__main__':
    unittest.main()
