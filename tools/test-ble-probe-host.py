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


@unittest.skipUnless(importlib.util.find_spec('bumble'), 'bumble is not installed')
class Bonds(unittest.TestCase):
    def test_each_pairing_gives_its_throwaway_address_as_its_identity(self):
        from bumble.pairing import PairingConfig, PairingDelegate
        config = host.pairing_config(PairingDelegate())
        self.assertEqual(config.identity_address_type, PairingConfig.AddressType.RANDOM)
        self.assertTrue(config.bonding and config.mitm and config.sc)


if __name__ == '__main__':
    unittest.main()
