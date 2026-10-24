package org.bevyengine.portalo;

import android.content.Context;
import android.net.Uri;
import android.provider.DocumentsContract;
import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;
import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.UUID;
import org.json.JSONObject;
import org.junit.Test;
import org.junit.runner.RunWith;
import static org.junit.Assert.*;
import static org.junit.Assume.*;

/** Uses only a newly created UUID task directory in the granted tree. */
@RunWith(AndroidJUnit4.class)
public class TransferDocumentsTest {
    @Test
    public void directTargetReopensAndResolvesRenamedDirectory() throws Exception {
        Context context=InstrumentationRegistry.getInstrumentation().getTargetContext();
        File config=new File(context.getFilesDir(),"settings.json");
        assumeTrue(config.isFile());
        JSONObject selected=new JSONObject(new String(Files.readAllBytes(config.toPath()),StandardCharsets.UTF_8)).optJSONObject("document_tree");
        assumeNotNull(selected);
        String tree=selected.getString("uri"), id=UUID.randomUUID().toString();
        assumeTrue("com.android.externalstorage.documents".equals(Uri.parse(tree).getAuthority()));
        File state=new File(context.getCacheDir(),"direct-test-"+id);
        assertTrue(state.mkdirs());
        Uri output=null;
        JSONObject manifest=new JSONObject().put("id",id).put("name","Portalo-direct-test-"+id).put("layout","Directory")
            .put("entries",new org.json.JSONArray()
                .put(new JSONObject().put("path",new org.json.JSONArray().put("empty")).put("directory",true).put("size",0).put("blocks",new org.json.JSONArray()))
                .put(new JSONObject().put("path",new org.json.JSONArray().put("payload")).put("directory",false).put("size",3).put("blocks",new org.json.JSONArray())));
        try {
            String descriptor=DirectTransferStorage.prepare(context,tree,state,manifest.toString());
            assumeFalse("null".equals(descriptor));
            JSONObject target=new JSONObject(descriptor);
            assertFalse(target.getBoolean("published"));
            Uri payload=Uri.parse(target.getJSONArray("uris").getString(1));
            int fd=DirectTransferStorage.open(context,payload,true);
            try(android.os.ParcelFileDescriptor owned=android.os.ParcelFileDescriptor.adoptFd(fd)) {
                byte[] bytes={1,7,9};
                assertEquals(3,android.system.Os.write(owned.getFileDescriptor(),bytes,0,3));
                android.system.Os.fsync(owned.getFileDescriptor());
            }
            JSONObject reopened=new JSONObject(DirectTransferStorage.prepare(context,tree,state,manifest.toString()));
            assertEquals(target.getString("root"),reopened.getString("root"));
            File journal=new File(state,"direct-target.json");
            output=Uri.parse(DirectTransferStorage.publish(context,state));
            JSONObject record=new JSONObject(new String(Files.readAllBytes(journal.toPath()),StandardCharsets.UTF_8));
            record.remove("result");record.put("root",target.getString("root"));
            Files.write(journal.toPath(),record.toString().getBytes(StandardCharsets.UTF_8));
            JSONObject recovered=new JSONObject(DirectTransferStorage.prepare(context,tree,state,manifest.toString()));
            assertTrue(recovered.getBoolean("published"));
            assertEquals(output.toString(),recovered.getString("root"));
            try(java.io.InputStream input=context.getContentResolver().openInputStream(Uri.parse(recovered.getJSONArray("uris").getString(1)))) {
                assertNotNull(input);
                assertEquals(1,input.read());assertEquals(7,input.read());assertEquals(9,input.read());assertEquals(-1,input.read());
            }
            // The Java descriptor intentionally does not mark Rust completion:
            // production recovery still verifies all BLAKE3 blocks afterward.
        } finally {
            if(output!=null) assertTrue(DocumentsContract.deleteDocument(context.getContentResolver(),output));
            else DirectTransferStorage.cancel(context,state);
            TransferDocuments.deleteLocal(state);
        }
    }
    @Test
    public void singleFileIsDirectCollisionSafeAndRecoversRename() throws Exception {
        Context context = InstrumentationRegistry.getInstrumentation().getTargetContext();
        JSONObject selected=new JSONObject(new String(Files.readAllBytes(new File(context.getFilesDir(),"settings.json").toPath()),StandardCharsets.UTF_8)).optJSONObject("document_tree");
        assumeNotNull(selected);
        String id=UUID.randomUUID().toString(), label="Portalo-file-test-"+id+".bin";
        File source=new File(context.getCacheDir(),"single-"+id);
        assertTrue(source.mkdirs());
        java.util.List<Uri> outputs=new java.util.ArrayList<>();
        byte[] bytes=new byte[1024*1024+17];new java.util.Random(17).nextBytes(bytes);
        try {
            Files.write(new File(source,label).toPath(),bytes);
            Files.write(new File(source,".portalo-publication.json").toPath(),new JSONObject().put("layout","File").toString().getBytes(StandardCharsets.UTF_8));
            String result=TransferDocuments.publish(context,selected.getString("uri"),source,label,id);
            outputs.add(Uri.parse(result));
            try(android.database.Cursor c=context.getContentResolver().query(outputs.get(0),new String[]{android.provider.OpenableColumns.DISPLAY_NAME,DocumentsContract.Document.COLUMN_MIME_TYPE},null,null,null)) {
                assertTrue(c.moveToFirst());assertEquals(label,c.getString(0));assertNotEquals(DocumentsContract.Document.MIME_TYPE_DIR,c.getString(1));
            }
            try(java.io.InputStream input=context.getContentResolver().openInputStream(outputs.get(0));java.io.ByteArrayOutputStream out=new java.io.ByteArrayOutputStream()) {
                byte[] chunk=new byte[65536];int n;while((n=input.read(chunk))!=-1)out.write(chunk,0,n);
                assertArrayEquals(bytes,out.toByteArray());
            }
            File journal=new File(source,".portalo-provider.json");
            JSONObject state=new JSONObject(new String(Files.readAllBytes(journal.toPath()),StandardCharsets.UTF_8));
            state.put("published",false);
            Files.write(journal.toPath(),state.toString().getBytes(StandardCharsets.UTF_8));
            assertEquals(result,TransferDocuments.publish(context,selected.getString("uri"),source,label,id));
            assertTrue(journal.delete());
            String second=TransferDocuments.publish(context,selected.getString("uri"),source,label,UUID.randomUUID().toString());
            outputs.add(Uri.parse(second));assertNotEquals(result,second);
            try(android.database.Cursor c=context.getContentResolver().query(outputs.get(1),new String[]{android.provider.OpenableColumns.DISPLAY_NAME},null,null,null)) {
                assertTrue(c.moveToFirst());assertEquals(label+".1",c.getString(0));
            }
        } finally {
            for(Uri uri:outputs) assertTrue(DocumentsContract.deleteDocument(context.getContentResolver(),uri));
            TransferDocuments.deleteLocal(source);
        }
    }
    @Test
    public void publishRecoverAndExportDirectory() throws Exception {
        Context context = InstrumentationRegistry.getInstrumentation().getTargetContext();
        File config = new File(context.getFilesDir(), "settings.json");
        assumeTrue(config.isFile());
        JSONObject selected = new JSONObject(new String(Files.readAllBytes(config.toPath()), StandardCharsets.UTF_8)).optJSONObject("document_tree");
        assumeNotNull(selected);
        Uri tree = Uri.parse(selected.getString("uri"));
        String id = UUID.randomUUID().toString();
        File source = new File(context.getCacheDir(), "v2-test-" + id);
        File exported = new File(context.getCacheDir(), "v2-export-" + id);
        Uri published = null;
        try {
            assertTrue(new File(source, "nested/empty").mkdirs());
            assertTrue(exported.mkdirs());
            Files.write(new File(source, "nested/payload").toPath(), new byte[] {1, 7, 9});
            Files.write(new File(source, "zero").toPath(), new byte[0]);
            String result = TransferDocuments.publish(context, tree.toString(), source, "Portalo-v2-test-" + id, id);
            published = Uri.parse(result);
            File journal = new File(source, ".portalo-provider.json");
            JSONObject state = new JSONObject(new String(Files.readAllBytes(journal.toPath()), StandardCharsets.UTF_8));
            state.put("published", false);
            Files.write(journal.toPath(), state.toString().getBytes(StandardCharsets.UTF_8));
            assertEquals(result, TransferDocuments.publish(context, tree.toString(), source, "Portalo-v2-test-" + id, id));
            TransferDocuments.exportSubtree(context, tree, published, exported, new MainActivity.Preparation());
            assertFalse(new File(exported, ".portalo-receipt").exists());
            assertTrue(new File(exported, "nested/empty").isDirectory());
            assertArrayEquals(new byte[] {1, 7, 9}, Files.readAllBytes(new File(exported, "nested/payload").toPath()));
            assertTrue(new File(exported, "zero").isFile());
            assertEquals(0, new File(exported, "zero").length());
        } finally {
            if (published != null) assertTrue(DocumentsContract.deleteDocument(context.getContentResolver(), published));
            TransferDocuments.deleteLocal(source);
            TransferDocuments.deleteLocal(exported);
        }
    }
}
