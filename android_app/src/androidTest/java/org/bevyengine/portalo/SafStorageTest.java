package org.bevyengine.portalo;

import android.content.ContentResolver;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.provider.DocumentsContract;
import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;
import org.json.JSONObject;
import org.junit.Before;
import org.junit.Test;
import org.junit.runner.RunWith;
import java.io.File;
import java.nio.file.Files;
import java.util.HashMap;
import java.util.Map;
import static org.junit.Assert.*;
import static org.junit.Assume.*;

/** Opt-in validation tests using a disposable Portalo-SAF-... folder. */
@RunWith(AndroidJUnit4.class)
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

    @Test
    public void validationLeavesNoProbe() throws Exception {
        Map<String, Uri> before = children();
        storage.validate(tree.toString());
        assertEquals(before, children());
        assertFalse(storage.label(tree.toString()).isEmpty());
    }

    @Test
    public void closedStorageRejectsValidationWithoutCreatingDocuments() throws Exception {
        Map<String, Uri> before = children();
        storage.close();
        try { storage.validate(tree.toString()); fail("Closed storage accepted validation"); }
        catch (java.io.IOException expected) { assertTrue(expected.getMessage().contains("关闭")); }
        assertEquals(before, children());
    }

    @Test
    public void revokedTestGrantIsRejectedWithReselectionMessage() throws Exception {
        assumeTrue("Explicit opt-in required", "true".equals(InstrumentationRegistry.getArguments().getString("revokeTestTree")));
        resolver.releasePersistableUriPermission(tree, Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
        try { storage.validate(tree.toString()); fail("Revoked folder must not be accepted"); }
        catch (java.io.IOException error) { assertTrue(error.getMessage().contains("重新选择")); }
    }
}
