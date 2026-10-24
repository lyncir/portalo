package org.bevyengine.portalo;

import android.content.Context;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract;
import android.system.Os;
import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;
import java.io.File;
import java.nio.file.Files;
import java.nio.charset.StandardCharsets;
import java.util.UUID;
import org.json.JSONObject;
import org.junit.Test;
import org.junit.runner.RunWith;
import static org.junit.Assert.*;
import static org.junit.Assume.*;

@RunWith(AndroidJUnit4.class)
public class SendDocumentsTest {
    @Test
    public void reopenLargeDocumentChecksRegionAndDeletion() throws Exception {
        Context context=InstrumentationRegistry.getInstrumentation().getTargetContext();
        File config=new File(context.getFilesDir(),"settings.json");
        assumeTrue(config.isFile());
        JSONObject settings=new JSONObject(new String(Files.readAllBytes(config.toPath()),StandardCharsets.UTF_8));
        assumeTrue(settings.has("document_tree") && !settings.isNull("document_tree"));
        Uri tree=Uri.parse(settings.getJSONObject("document_tree").getString("uri"));
        Uri parent=DocumentsContract.buildDocumentUriUsingTree(tree,DocumentsContract.getTreeDocumentId(tree));
        Uri document=DocumentsContract.createDocument(context.getContentResolver(),parent,"application/octet-stream","Portalo-source-test-"+UUID.randomUUID()+".bin");
        assertNotNull(document);
        long size=8L*1024*1024*1024;
        try {
            try (ParcelFileDescriptor output=context.getContentResolver().openFileDescriptor(document,"rw")) {
                assertNotNull(output);
                Os.ftruncate(output.getFileDescriptor(),size);
                Os.fsync(output.getFileDescriptor());
            }
            for (int i=0;i<2;i++) {
                try (ParcelFileDescriptor input=ParcelFileDescriptor.adoptFd(SendDocuments.open(context,document,0,size))) {
                    assertEquals(size,Os.fstat(input.getFileDescriptor()).st_size);
                    byte[] tail=new byte[1];
                    assertEquals(1,Os.pread(input.getFileDescriptor(),tail,0,1,size-1));
                    assertEquals(0,tail[0]);
                }
            }
            try { ParcelFileDescriptor.adoptFd(SendDocuments.open(context,document,1,size-1)).close();fail("Changed region accepted"); }
            catch (java.io.IOException expected) { }
            assertTrue(DocumentsContract.deleteDocument(context.getContentResolver(),document));
            try { ParcelFileDescriptor.adoptFd(SendDocuments.open(context,document,0,size)).close();fail("Deleted source accepted"); }
            catch (java.io.IOException | IllegalArgumentException | SecurityException expected) { }
        } finally { try { DocumentsContract.deleteDocument(context.getContentResolver(),document); } catch (Exception ignored) { } }
    }
}
