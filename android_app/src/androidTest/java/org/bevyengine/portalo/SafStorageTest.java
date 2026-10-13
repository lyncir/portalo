package org.bevyengine.portalo;

import android.content.ContentResolver;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract;
import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;
import org.json.JSONObject;
import org.junit.Before;
import org.junit.FixMethodOrder;
import org.junit.Test;
import org.junit.runner.RunWith;
import org.junit.runners.MethodSorters;
import java.io.File;
import java.io.InputStream;
import java.nio.file.Files;
import java.util.HashMap;
import java.util.Map;
import java.util.UUID;
import static org.junit.Assert.*;
import static org.junit.Assume.*;

/** Opt-in real-provider tests. Select a disposable "Portalo-SAF-..." folder
 * in the app first. The optional revokeTestTree=true case revokes only that
 * test folder and requires selecting it again afterwards.
 */
@RunWith(AndroidJUnit4.class)
@FixMethodOrder(MethodSorters.NAME_ASCENDING)
public class SafStorageTest {
    private SafStorage storage;
    private ContentResolver resolver;
    private Uri tree;

    @Before
    public void selectedTestFolder() throws Exception {
        android.content.Context context = InstrumentationRegistry.getInstrumentation().getTargetContext();
        File config = new File(context.getFilesDir(), "settings.json");
        assumeTrue("Select a disposable SAF test folder first", config.isFile());
        JSONObject selected = new JSONObject(new String(Files.readAllBytes(config.toPath()), java.nio.charset.StandardCharsets.UTF_8)).optJSONObject("document_tree");
        assumeNotNull(selected);
        assumeTrue("Never run against a user's ordinary receive folder", selected.getString("label").startsWith("Portalo-SAF-"));
        tree = Uri.parse(selected.getString("uri"));
        storage = new SafStorage(context);
        resolver = context.getContentResolver();
    }

    private Map<String, Uri> children() throws Exception {
        Map<String, Uri> result = new HashMap<>();
        Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree));
        try (Cursor cursor = resolver.query(children, new String[] {
                DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_DOCUMENT_ID }, null, null, null)) {
            assertNotNull(cursor);
            while (cursor.moveToNext()) result.put(cursor.getString(0), DocumentsContract.buildDocumentUriUsingTree(tree, cursor.getString(1)));
        }
        return result;
    }

    private void write(long token, byte[] bytes) throws Exception {
        try (ParcelFileDescriptor.AutoCloseOutputStream output = new ParcelFileDescriptor.AutoCloseOutputStream(
                ParcelFileDescriptor.adoptFd(storage.duplicateFd(token)))) { output.write(bytes); }
    }

    private byte[] read(Uri uri) throws Exception {
        assertNotNull(uri);
        try (InputStream input = resolver.openInputStream(uri);
             java.io.ByteArrayOutputStream output = new java.io.ByteArrayOutputStream()) {
            assertNotNull(input);
            byte[] buffer = new byte[4096]; int count;
            while ((count = input.read(buffer)) != -1) output.write(buffer, 0, count);
            return output.toByteArray();
        }
    }

    @Test
    public void a_commitPreservesSameNamesAndExactBytesIncludingEmptyFile() throws Exception {
        String name = "saf-test-中文-" + UUID.randomUUID() + ".bin";
        long a = storage.begin(tree.toString(), name);
        long b = storage.begin(tree.toString(), name);
        try {
            write(a, new byte[] {1, 2, 3}); write(b, new byte[0]);
            storage.finish(a); storage.finish(b);
            Map<String, Uri> entries = children();
            assertArrayEquals(new byte[] {1, 2, 3}, read(entries.get(name)));
            assertArrayEquals(new byte[0], read(entries.get(name + ".1")));
        } finally {
            storage.abort(a); storage.abort(b);
            for (Map.Entry<String, Uri> entry : children().entrySet()) {
                if (entry.getKey().equals(name) || entry.getKey().equals(name + ".1")) {
                    assertTrue(DocumentsContract.deleteDocument(resolver, entry.getValue()));
                }
            }
        }
    }

    @Test
    public void b_abortRemovesPartialDocumentAndValidationLeavesNoProbe() throws Exception {
        Map<String, Uri> before = children();
        storage.validate(tree.toString());
        assertEquals(before, children());
        long token = storage.begin(tree.toString(), "aborted-" + UUID.randomUUID());
        try { write(token, new byte[] {1,2,3}); }
        finally { storage.abort(token); }
        storage.abort(token);
        assertEquals(before, children());
    }

    @Test
    public void z_revokedTestGrantIsRejectedWithReselectionMessage() throws Exception {
        assumeTrue("Explicit opt-in required", "true".equals(InstrumentationRegistry.getArguments().getString("revokeTestTree")));
        resolver.releasePersistableUriPermission(tree, Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
        try { storage.validate(tree.toString()); fail("Revoked folder must not be accepted"); }
        catch (java.io.IOException error) { assertTrue(error.getMessage().contains("重新选择")); }
    }
}
