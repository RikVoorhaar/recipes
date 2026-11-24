from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

OPS = Path(__file__).resolve().parents[1] / "scripts" / "ops.py"


class OperationsCommands(unittest.TestCase):
    def test_documented_backup_restore_preserves_history_image_bytes(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            live = root / "live"
            (live / "content").mkdir(parents=True)
            db = live / "recipes.sqlite3"
            historical_bytes = b"\xff\xd8historical-image\x00bytes\xff\xd9"
            image = live / "content" / "img_history"
            image.write_bytes(historical_bytes)
            with sqlite3.connect(db) as connection:
                connection.executescript("""
                    CREATE TABLE recipes(id TEXT PRIMARY KEY, current_revision_id TEXT);
                    CREATE TABLE recipe_revisions(id TEXT PRIMARY KEY, recipe_id TEXT, image_ref TEXT);
                    INSERT INTO recipes VALUES ('rcp_fixture', 'rev_current');
                    INSERT INTO recipe_revisions VALUES ('rev_old', 'rcp_fixture', 'img_history');
                    INSERT INTO recipe_revisions VALUES ('rev_current', 'rcp_fixture', NULL);
                """)
            bundle, restored = root / "backup", root / "restored"
            subprocess.run([sys.executable, str(OPS), "backup", "--database", str(db),
                            "--content", str(live / "content"), "--output", str(bundle)], check=True)
            subprocess.run([sys.executable, str(OPS), "restore", "--backup", str(bundle),
                            "--destination", str(restored)], check=True)
            with sqlite3.connect(restored / "recipes.sqlite3") as connection:
                self.assertEqual(connection.execute(
                    "SELECT image_ref FROM recipe_revisions WHERE id='rev_old'"
                ).fetchone()[0], "img_history")
            self.assertEqual((restored / "content" / "img_history").read_bytes(), historical_bytes)

    def test_restore_refuses_existing_destination_and_tampered_bundle(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            source, content = root / "db", root / "content"
            content.mkdir()
            with sqlite3.connect(source) as connection:
                connection.execute("CREATE TABLE sample(value TEXT)")
            bundle, dest = root / "backup", root / "existing"
            subprocess.run([sys.executable, str(OPS), "backup", "--database", str(source),
                            "--content", str(content), "--output", str(bundle)], check=True)
            dest.mkdir()
            result = subprocess.run([sys.executable, str(OPS), "restore", "--backup", str(bundle),
                                     "--destination", str(dest)], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(dest.is_dir())
            (bundle / "content.tar.gz").write_bytes(b"tampered")
            result = subprocess.run([sys.executable, str(OPS), "restore", "--backup", str(bundle),
                                     "--destination", str(root / "nope")], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((root / "nope").exists())

    def test_backup_rejects_resolved_overlaps_and_symlink_aliases(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            content = root / "live" / "content"
            content.mkdir(parents=True)
            db = root / "live" / "recipes.sqlite3"
            with sqlite3.connect(db) as connection:
                connection.execute("CREATE TABLE sample(value TEXT)")
            for output in (content / "inside", root / "live", root / "alias"):
                if output.name == "alias":
                    output.symlink_to(content, target_is_directory=True)
                result = subprocess.run(
                    [sys.executable, str(OPS), "backup", "--database", str(db),
                     "--content", str(content), "--output", str(output)],
                    capture_output=True,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((content / "inside").exists())
                self.assertEqual(set((root / "live").iterdir()), {db, content})
                if output.name == "alias":
                    self.assertTrue(output.is_symlink())

    def test_dev_copy_requires_new_separate_destination(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            content, db, dest = root / "content", root / "db.sqlite3", root / "dev"
            content.mkdir()
            (content / "asset").write_bytes(b"asset bytes")
            with sqlite3.connect(db) as connection:
                connection.execute("CREATE TABLE data(value TEXT)")
                connection.execute("INSERT INTO data VALUES ('fixture')")
                connection.execute("CREATE TABLE api_tokens(token_hash TEXT, label TEXT)")
                connection.execute("INSERT INTO api_tokens VALUES ('production-secret-hash', 'prod')")
            subprocess.run([sys.executable, str(OPS), "dev-copy", "--database", str(db),
                            "--content", str(content), "--destination", str(dest)], check=True)
            self.assertEqual((dest / "content" / "asset").read_bytes(), b"asset bytes")
            with sqlite3.connect(dest / "recipes.sqlite3") as connection:
                self.assertEqual(connection.execute("SELECT COUNT(*) FROM api_tokens").fetchone()[0], 0)
            result = subprocess.run([sys.executable, str(OPS), "dev-copy", "--database", str(db),
                                     "--content", str(content), "--destination", str(dest)],
                                    capture_output=True)
            self.assertNotEqual(result.returncode, 0)

    def test_dev_copy_scrub_failure_does_not_publish_partial_destination(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            content, db, dest = root / "content", root / "db.sqlite3", root / "dev"
            content.mkdir()
            with sqlite3.connect(db) as connection:
                connection.execute("CREATE TABLE application_data(value TEXT)")
            result = subprocess.run(
                [sys.executable, str(OPS), "dev-copy", "--database", str(db),
                 "--content", str(content), "--destination", str(dest)],
                capture_output=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(dest.exists())
            self.assertEqual(list(root.glob(".recipe-dev-copy-*")), [])

    def test_dev_copy_rejects_ancestor_descendant_and_symlink_overlap(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            content, db = root / "content", root / "db.sqlite3"
            content.mkdir()
            with sqlite3.connect(db) as connection:
                connection.execute("CREATE TABLE api_tokens(token_hash TEXT, label TEXT)")
            alias = root / "content-link"
            alias.symlink_to(content, target_is_directory=True)
            db_alias = root / "database-link"
            db_alias.symlink_to(db)
            for destination in (
                content,
                content / "child",
                db,
                root,
                root / "content-link" / "child",
                db_alias,
            ):
                result = subprocess.run(
                    [sys.executable, str(OPS), "dev-copy", "--database", str(db),
                     "--content", str(content), "--destination", str(destination)],
                    capture_output=True,
                )
                self.assertNotEqual(result.returncode, 0)
            self.assertEqual(list(content.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
