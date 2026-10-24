package org.bevyengine.portalo;

import android.content.Context;
import android.database.Cursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract;
import android.system.Os;
import android.system.OsConstants;
import android.util.AtomicFile;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.util.*;
import org.json.JSONArray;
import org.json.JSONObject;

/** Local SAF direct backend. Durable metadata lives in the app task directory.
 * No handles are retained across JNI calls/restarts. Capabilities are actually
 * exercised on a disposable object, not inferred solely from provider flags.
 */
final class DirectTransferStorage {
    private static final String DIR=DocumentsContract.Document.MIME_TYPE_DIR;
    private static File journal(File state) { return new File(state,"direct-target.json"); }
    private static boolean exists(File state) {
        return journal(state).exists() || new File(state,"direct-target.json.bak").exists();
    }
    private static JSONObject load(File state) throws Exception {
        return new JSONObject(new String(new AtomicFile(journal(state)).readFully(),StandardCharsets.UTF_8));
    }
    private static void save(File state,JSONObject value) throws Exception {
        AtomicFile file=new AtomicFile(journal(state)); FileOutputStream output=file.startWrite();
        try { output.write(value.toString().getBytes(StandardCharsets.UTF_8));file.finishWrite(output); }
        catch(Exception error) {file.failWrite(output);throw error;}
    }
    private static Uri parent(Uri tree) {
        return DocumentsContract.buildDocumentUriUsingTree(tree,DocumentsContract.getTreeDocumentId(tree));
    }
    private static Map<String,Uri> children(Context context,Uri tree,Uri root) throws Exception {
        Map<String,Uri> result=new HashMap<>();
        Uri query=DocumentsContract.buildChildDocumentsUriUsingTree(tree,DocumentsContract.getDocumentId(root));
        try(Cursor cursor=context.getContentResolver().query(query,new String[]{DocumentsContract.Document.COLUMN_DISPLAY_NAME,DocumentsContract.Document.COLUMN_DOCUMENT_ID},null,null,null)) {
            if(cursor==null) throw new IOException("Cannot enumerate direct target");
            while(cursor.moveToNext()) {
                if(result.size()>=100001) throw new IOException("Too many target entries");
                String name=cursor.getString(0);
                if(result.put(name,DocumentsContract.buildDocumentUriUsingTree(tree,cursor.getString(1)))!=null) throw new IOException("Ambiguous target name: "+name);
            }
        }
        return result;
    }
    private static boolean directory(Context context,Uri uri) throws Exception {
        try(Cursor cursor=context.getContentResolver().query(uri,new String[]{DocumentsContract.Document.COLUMN_MIME_TYPE},null,null,null)) {
            if(cursor==null || !cursor.moveToFirst()) throw new IOException("Target disappeared");
            return DIR.equals(cursor.getString(0));
        }
    }
    private static Uri create(Context context,Uri parent,String name,boolean directory) throws Exception {
        Uri uri=DocumentsContract.createDocument(context.getContentResolver(),parent,directory?DIR:"application/octet-stream",name);
        if(uri==null) throw new IOException("Cannot create direct target: "+name);
        try(Cursor cursor=context.getContentResolver().query(uri,new String[]{DocumentsContract.Document.COLUMN_DISPLAY_NAME},null,null,null)) {
            if(cursor==null || !cursor.moveToFirst() || !name.equals(cursor.getString(0))) {
                remove(context,uri);
                throw new IOException("Provider changed temporary entry name: "+name);
            }
        }
        return uri;
    }
    private static void remove(Context context,Uri uri) throws Exception {
        if(!DocumentsContract.deleteDocument(context.getContentResolver(),uri)) throw new IOException("Cannot remove temporary target: "+uri);
    }
    private static boolean capable(Context context,Uri tree,String id) throws Exception {
        // First release is intentionally restricted to the Android local
        // storage provider. Remote/FUSE-like providers retain staged fallback.
        if(!"com.android.externalstorage.documents".equals(tree.getAuthority())) return false;
        Uri probe=null; boolean supported=false;
        try {
            probe=create(context,parent(tree),".portalo-probe-"+id,true);
            Uri file=create(context,probe,"probe",false);
            try(ParcelFileDescriptor descriptor=context.getContentResolver().openFileDescriptor(file,"rw")) {
                if(descriptor==null) throw new IOException("Missing probe FD");
                java.io.FileDescriptor fd=descriptor.getFileDescriptor();
                if(!OsConstants.S_ISREG(Os.fstat(fd).st_mode)) throw new IOException("Non-regular provider FD");
                byte[] data={17,33};
                if(Os.write(fd,data,0,data.length)!=data.length) throw new IOException("Short probe write");
                Os.lseek(fd,0,OsConstants.SEEK_SET);
                byte[] read=new byte[2];
                if(Os.read(fd,read,0,2)!=2 || !Arrays.equals(data,read)) throw new IOException("Probe read mismatch");
                Os.ftruncate(fd,1);Os.fsync(fd);
                if(Os.fstat(fd).st_size!=1) throw new IOException("Truncate unsupported");
            }
            if(DocumentsContract.renameDocument(context.getContentResolver(),file,"renamed")==null) throw new IOException("File rename unsupported");
            Uri renamed=DocumentsContract.renameDocument(context.getContentResolver(),probe,".portalo-probe-"+id+"-renamed");
            if(renamed==null) throw new IOException("Directory rename unsupported");
            probe=renamed;supported=true;
        } catch(Exception error) {
            android.util.Log.i("PortaloStorage","Direct SAF probe failed; use staging",error);
        } finally {
            // Never hide cleanup failures behind an automatic fallback.
            if(probe!=null) remove(context,probe);
        }
        return supported;
    }
    static String prepare(Context context,String treeValue,File state,String manifestJson) throws Exception {
        JSONObject manifest=new JSONObject(manifestJson);
        Uri tree=Uri.parse(treeValue); String id=manifest.getString("id");
        JSONObject record;
        if(exists(state)) record=load(state);
        else {
            boolean direct=capable(context,tree,id);
            record=new JSONObject().put("tree",treeValue).put("manifest",manifest)
                .put("mode",direct?"direct":"staged").put("temporary",".portalo-"+id+".part");
            save(state,record);
        }
        if(!treeValue.equals(record.getString("tree"))) throw new IOException("Direct target tree changed");
        if(!"direct".equals(record.getString("mode"))) return "null";
        boolean single="File".equals(manifest.optString("layout"));
        Map<String,Uri> siblings=children(context,tree,parent(tree));
        Uri temporary=siblings.get(record.getString("temporary"));
        boolean published=false;Uri root;
        if(record.has("result")) { root=Uri.parse(record.getString("result"));published=true; }
        else if(record.optBoolean("publishing") && temporary==null) {
            root=siblings.get(record.getString("target"));
            if(root==null) throw new IOException("Publication outcome unknown; neither temporary nor final document exists");
            published=true;
        } else if(temporary!=null) {
            if(!record.optBoolean("creating")) throw new IOException("Unclaimed temporary document");
            root=temporary;
        } else {
            if(record.has("root")) throw new IOException("Temporary document deleted; cancel task before retrying");
            record.put("creating",true);save(state,record);
            root=create(context,parent(tree),record.getString("temporary"),!single);
        }
        if(directory(context,root)==single) throw new IOException("Direct root type mismatch");
        record.put("root",root.toString());save(state,record);
        JSONArray entries=manifest.getJSONArray("entries"),uris=new JSONArray();
        Map<String,Uri> directories=new HashMap<>();directories.put("",root);
        Map<String,Map<String,Uri>> listing=new HashMap<>();
        Set<String> expected=new HashSet<>();
        for(int i=0;i<entries.length();i++) {
            JSONObject entry=entries.getJSONObject(i);JSONArray path=entry.getJSONArray("path");
            String key="",parentKey="";
            for(int j=0;j<path.length();j++) {parentKey=key;key+="/"+path.getString(j);}
            expected.add(key);
            Uri uri;
            if(single) uri=root;
            else {
                Uri parent=directories.get(parentKey);
                if(parent==null) throw new IOException("Missing manifest parent");
                Map<String,Uri> names=listing.get(parentKey);
                if(names==null) { names=children(context,tree,parent);listing.put(parentKey,names); }
                String name=path.getString(path.length()-1);uri=names.get(name);
                if(uri==null) {
                    if(published) throw new IOException("Published directory is missing "+key);
                    uri=create(context,parent,name,entry.getBoolean("directory"));names.put(name,uri);
                }
            }
            if(directory(context,uri)!=entry.getBoolean("directory")) throw new IOException("Target entry type mismatch: "+key);
            if(entry.getBoolean("directory")) directories.put(key,uri);
            uris.put(uri.toString());
        }
        if(!single) {
            Set<String> actual=new HashSet<>();inventory(context,tree,root,"",actual,entries.length(),0);
            if(!actual.equals(expected)) throw new IOException("Target directory inventory mismatch");
        }
        return new JSONObject().put("root",root.toString()).put("uris",uris).put("published",published).toString();
    }
    private static void inventory(Context context,Uri tree,Uri root,String prefix,Set<String> paths,int limit,int depth) throws Exception {
        if(depth>64) throw new IOException("Target too deep");
        for(Map.Entry<String,Uri> entry:children(context,tree,root).entrySet()) {
            String path=prefix+"/"+entry.getKey();paths.add(path);
            if(paths.size()>limit) throw new IOException("Unexpected target entries");
            if(directory(context,entry.getValue())) inventory(context,tree,entry.getValue(),path,paths,limit,depth+1);
        }
    }
    static int open(Context context,Uri uri,boolean write) throws Exception {
        try(ParcelFileDescriptor descriptor=context.getContentResolver().openFileDescriptor(uri,write?"rw":"r")) {
            if(descriptor==null) throw new IOException("Cannot open target document");
            if(!OsConstants.S_ISREG(Os.fstat(descriptor.getFileDescriptor()).st_mode)) throw new IOException("Provider FD capability changed");
            Os.lseek(descriptor.getFileDescriptor(),0,OsConstants.SEEK_SET);
            return descriptor.detachFd();
        }
    }
    static String publish(Context context,File state) throws Exception {
        JSONObject record=load(state);String treeValue=record.getString("tree");
        synchronized(TransferDocuments.treeLock(treeValue)) {
            if(record.has("result")) return record.getString("result");
            Uri tree=Uri.parse(treeValue), root=Uri.parse(record.getString("root"));
            Map<String,Uri> siblings=children(context,tree,parent(tree));
            if(record.optBoolean("publishing") && !siblings.containsKey(record.getString("temporary"))) {
                // Recovery must go through prepare + Rust BLAKE3 verification.
                throw new IOException("Resume to verify publication outcome");
            }
            JSONObject manifest=record.getJSONObject("manifest");String label=manifest.getString("name");
            if("Legacy".equals(manifest.optString("layout","Legacy"))) label+="-"+manifest.getString("id").substring(0,8);
            String target=label;int suffix=0;
            while(siblings.containsKey(target)) target=label+"."+(++suffix);
            record.put("target",target).put("publishing",true);save(state,record);
            Uri result=DocumentsContract.renameDocument(context.getContentResolver(),root,target);
            if(result==null) throw new IOException("Cannot publish direct target");
            record.put("root",result.toString()).put("result",result.toString());save(state,record);
            return result.toString();
        }
    }
    static void cancel(Context context,File state) throws Exception {
        if(!exists(state)) return;
        JSONObject record=load(state);
        if(record.has("result") || record.optBoolean("publishing")) throw new IOException("Resume to resolve publication before cancelling");
        Uri tree=Uri.parse(record.getString("tree"));
        Uri root=children(context,tree,parent(tree)).get(record.getString("temporary"));
        if(root!=null) remove(context,root);
        new AtomicFile(journal(state)).delete();
    }
}
