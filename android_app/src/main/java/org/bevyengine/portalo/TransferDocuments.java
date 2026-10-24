package org.bevyengine.portalo;

import android.content.Context;
import android.content.ClipData;
import android.net.Uri;
import android.database.Cursor;
import android.provider.DocumentsContract;
import android.provider.OpenableColumns;
import android.content.res.AssetFileDescriptor;
import android.util.AtomicFile;
import java.io.*;
import java.nio.file.Files;
import java.util.*;
import org.json.JSONObject;

/** V2 snapshot export and recoverable whole-directory SAF publication. */
final class TransferDocuments {
    private static final Map<String,Object> LOCKS=new java.util.concurrent.ConcurrentHashMap<>();
    static Object treeLock(String tree) { return LOCKS.computeIfAbsent(tree,key->new Object()); }
    private static final Map<String,java.util.concurrent.atomic.AtomicLong> PROGRESS=new java.util.concurrent.ConcurrentHashMap<>();
    static long progress(String id) { java.util.concurrent.atomic.AtomicLong value=PROGRESS.get(id); return value==null?0:value.get(); }
    static void deleteLocal(File file) {
        File[] children=file.listFiles();
        if (children!=null) for(File child:children) deleteLocal(child);
        file.delete();
    }
    private static String name(Context context,Uri uri) throws Exception {
        try(Cursor cursor=context.getContentResolver().query(uri,new String[]{OpenableColumns.DISPLAY_NAME},null,null,null)) {
            if(cursor==null || !cursor.moveToFirst()) throw new IOException("Cannot read document name");
            String name=cursor.getString(0);
            if(name==null || name.isEmpty() || name.equals(".") || name.equals("..") || name.contains("/") || name.contains("\\")) throw new IOException("Invalid document name");
            return name;
        }
    }
    static String exportTree(Context context,Uri tree,File root,MainActivity.Preparation job) throws Exception {
        Uri document=DocumentsContract.buildDocumentUriUsingTree(tree,DocumentsContract.getTreeDocumentId(tree));
        return exportSubtree(context,tree,document,root,job);
    }
    static String exportSubtree(Context context,Uri tree,Uri document,File root,MainActivity.Preparation job) throws Exception {
        String label=name(context,document);
        export(context,tree,document,root,job,0,new int[]{0},new HashSet<>());
        return label;
    }
    private static void export(Context context,Uri tree,Uri parent,File target,MainActivity.Preparation job,int depth,int[] count,Set<String> visited) throws Exception {
        job.check();
        if(depth>64 || !visited.add(parent.toString())) throw new IOException("Directory depth/cycle limit");
        Uri children=DocumentsContract.buildChildDocumentsUriUsingTree(tree,DocumentsContract.getDocumentId(parent));
        try(Cursor cursor=context.getContentResolver().query(children,new String[]{DocumentsContract.Document.COLUMN_DOCUMENT_ID,DocumentsContract.Document.COLUMN_DISPLAY_NAME,DocumentsContract.Document.COLUMN_MIME_TYPE},null,null,null,job.signal)) {
            if(cursor==null) throw new IOException("Cannot enumerate directory");
            while(cursor.moveToNext()) {
                job.check(); if(++count[0]>100000) throw new IOException("Too many entries");
                Uri child=DocumentsContract.buildDocumentUriUsingTree(tree,cursor.getString(0));
                String name=name(context,child);
                File output=new File(target,name);
                if(output.exists()) throw new IOException("Duplicate document name");
                if(DocumentsContract.Document.MIME_TYPE_DIR.equals(cursor.getString(2))) {
                    if(!output.mkdir()) throw new IOException("Cannot create directory");
                    export(context,tree,child,output,job,depth+1,count,visited);
                } else copy(context,child,output,job);
            }
        }
    }
    static void exportFiles(Context context,ClipData data,File root,MainActivity.Preparation job) throws Exception {
        if(data.getItemCount()>100000) throw new IOException("Too many files");
        for(int i=0;i<data.getItemCount();i++) {
            Uri uri=data.getItemAt(i).getUri();
            File output=new File(root,name(context,uri));
            if(output.exists()) throw new IOException("Duplicate filename");
            copy(context,uri,output,job);
        }
    }
    private static void copy(Context context,Uri uri,File output,MainActivity.Preparation job) throws Exception {
        try(AssetFileDescriptor asset=context.getContentResolver().openAssetFileDescriptor(uri,"r",job.signal);
            FileOutputStream stream=new FileOutputStream(output)) {
            MainActivity.copyToCache(job,asset,stream);
            stream.getFD().sync();
        }
    }
    private static Uri child(Context context,Uri tree,Uri parent,String name) throws Exception {
        Uri found=null;
        Uri children=DocumentsContract.buildChildDocumentsUriUsingTree(tree,DocumentsContract.getDocumentId(parent));
        try(Cursor cursor=context.getContentResolver().query(children,new String[]{DocumentsContract.Document.COLUMN_DOCUMENT_ID,DocumentsContract.Document.COLUMN_DISPLAY_NAME},null,null,null)) {
            if(cursor==null) throw new IOException("Cannot query destination");
            while(cursor.moveToNext()) if(name.equals(cursor.getString(1))) {
                if(found!=null) throw new IOException("Ambiguous destination name");
                found=DocumentsContract.buildDocumentUriUsingTree(tree,cursor.getString(0));
            }
        }
        return found;
    }
    private static void persist(File path,JSONObject state) throws Exception {
        AtomicFile file=new AtomicFile(path);
        FileOutputStream stream=file.startWrite();
        try { stream.write(state.toString().getBytes(java.nio.charset.StandardCharsets.UTF_8)); file.finishWrite(stream); }
        catch(Exception error) {file.failWrite(stream);throw error;}
    }
    private static JSONObject load(File path) throws Exception {
        AtomicFile file=new AtomicFile(path);
        return new JSONObject(new String(file.readFully(),java.nio.charset.StandardCharsets.UTF_8));
    }
    static String publish(Context context,String treeValue,File source,String label,String id) throws Exception {
        synchronized(treeLock(treeValue)) {
            Uri tree=Uri.parse(treeValue);
            Uri parent=DocumentsContract.buildDocumentUriUsingTree(tree,DocumentsContract.getTreeDocumentId(tree));
            File journal=new File(source,".portalo-provider.json");
            File metadata=new File(source,".portalo-publication.json");
            JSONObject publication=metadata.exists()?new JSONObject(new String(Files.readAllBytes(metadata.toPath()),java.nio.charset.StandardCharsets.UTF_8)):new JSONObject();
            boolean single="File".equals(publication.optString("layout"));
            boolean legacy=!publication.has("layout") || "Legacy".equals(publication.optString("layout"));
            JSONObject state=journal.exists()?load(journal):new JSONObject();
            String target=state.optString("target",legacy?label+"-"+id.substring(0,8):label);
            if(!state.has("target") && !legacy) {
                int suffix=0;
                while(child(context,tree,parent,target)!=null) target=label+"."+(++suffix);
                state.put("target",target);persist(journal,state);
            }
            java.util.concurrent.atomic.AtomicLong progress=new java.util.concurrent.atomic.AtomicLong();
            PROGRESS.put(id,progress);
            try {
            if(state.optBoolean("published")) return state.getString("uri");
            Uri existing=child(context,tree,parent,target);
            if(existing!=null && !legacy && (!state.optBoolean("ready") || child(context,tree,parent,state.optString("temporaryName",".portalo-"+id+".part"))!=null)) {
                int suffix=0;
                do { target=label+"."+(++suffix); } while(child(context,tree,parent,target)!=null);
                state.put("target",target);persist(journal,state);
                existing=null;
            }
            if(existing!=null) {
                if(!state.optBoolean("ready")) throw new IOException("Destination already exists");
                if(single) {
                    try(InputStream input=context.getContentResolver().openInputStream(existing); InputStream expected=new FileInputStream(new File(source,label))) {
                        if(!Arrays.equals(digest(input),digest(expected))) throw new IOException("Publication identity mismatch");
                    }
                } else {
                compareDirectory(context,tree,source,existing);
                }
                state.put("published",true).put("uri",existing.toString());persist(journal,state);return existing.toString();
            }
            Uri temporary;
            if(state.has("uri")) temporary=Uri.parse(state.getString("uri"));
            else {
                String temporaryName=".portalo-"+id+".part";
                temporary=child(context,tree,parent,temporaryName);
                if(temporary!=null && !state.optBoolean("creating")) throw new IOException("Unclaimed temporary directory; manual cleanup required");
                if(temporary==null) {
                    state.put("creating",true).put("tree",treeValue).put("temporaryName",temporaryName);
                    persist(journal,state);
                    temporary=DocumentsContract.createDocument(context.getContentResolver(),parent,single?"application/octet-stream":DocumentsContract.Document.MIME_TYPE_DIR,temporaryName);
                }
                if(temporary==null) throw new IOException("Cannot create task directory");
                state.put("uri",temporary.toString());persist(journal,state);
            }
            if(!state.optBoolean("ready")) {
                if(single) copyPublishedFile(context,new File(source,label),temporary,progress);
                else copyDirectory(context,tree,source,temporary,progress);
            }
            state.put("ready",true);persist(journal,state);
            // Recheck external collisions immediately before publication.
            if(child(context,tree,parent,target)!=null) {
                int suffix=0;
                do { target=label+"."+(++suffix); } while(child(context,tree,parent,target)!=null);
                state.put("target",target);persist(journal,state);
            }
            Uri renamed=DocumentsContract.renameDocument(context.getContentResolver(),temporary,target);
            if(renamed==null) throw new IOException("Cannot publish directory");
            state.put("published",true).put("uri",renamed.toString());persist(journal,state);
            return renamed.toString();
            } finally { PROGRESS.remove(id); }
        }
    }
    private static byte[] digest(InputStream input) throws Exception {
        if(input==null) throw new IOException("Cannot read published document");
        java.security.MessageDigest digest=java.security.MessageDigest.getInstance("SHA-256");
        byte[] bytes=new byte[256*1024];int n;
        while((n=input.read(bytes))!=-1) digest.update(bytes,0,n);
        return digest.digest();
    }
    private static boolean internal(String name) {
        return name.equals(".portalo-publication.json") || name.startsWith(".portalo-provider");
    }
    private static void compareDirectory(Context context,Uri tree,File source,Uri target) throws Exception {
        Map<String,Uri> actual=new HashMap<>();
        Map<String,Boolean> types=new HashMap<>();
        Uri query=DocumentsContract.buildChildDocumentsUriUsingTree(tree,DocumentsContract.getDocumentId(target));
        try(Cursor cursor=context.getContentResolver().query(query,new String[]{DocumentsContract.Document.COLUMN_DISPLAY_NAME,DocumentsContract.Document.COLUMN_DOCUMENT_ID,DocumentsContract.Document.COLUMN_MIME_TYPE},null,null,null)) {
            if(cursor==null) throw new IOException("Cannot verify publication inventory");
            while(cursor.moveToNext()) {
                String name=cursor.getString(0);
                if(actual.size()>=100000) throw new IOException("Unexpected publication entries");
                Uri uri=DocumentsContract.buildDocumentUriUsingTree(tree,cursor.getString(1));
                if(actual.put(name,uri)!=null) throw new IOException("Ambiguous publication name");
                types.put(name,DocumentsContract.Document.MIME_TYPE_DIR.equals(cursor.getString(2)));
            }
        }
        File[] expected=source.listFiles();if(expected==null) throw new IOException("Cannot verify source inventory");
        for(File file:expected) {
            if(internal(file.getName())) continue;
            Uri uri=actual.remove(file.getName());
            if(uri==null || types.get(file.getName())!=file.isDirectory()) throw new IOException("Publication inventory mismatch: "+file.getName());
            if(file.isDirectory()) compareDirectory(context,tree,file,uri);
            else try(InputStream input=context.getContentResolver().openInputStream(uri); InputStream original=new FileInputStream(file)) {
                if(!Arrays.equals(digest(input),digest(original))) throw new IOException("Publication checksum mismatch: "+file.getName());
            }
        }
        if(!actual.isEmpty()) throw new IOException("Unexpected published files");
    }
    private static void copyPublishedFile(Context context,File file,Uri uri,java.util.concurrent.atomic.AtomicLong progress) throws Exception {
        try(FileInputStream input=new FileInputStream(file);
            android.os.ParcelFileDescriptor descriptor=context.getContentResolver().openFileDescriptor(uri,"wt")) {
            if(descriptor==null) throw new IOException("Cannot open destination");
            try(FileOutputStream output=new android.os.ParcelFileDescriptor.AutoCloseOutputStream(descriptor)) {
                java.nio.channels.FileChannel from=input.getChannel(), to=output.getChannel();
                long offset=0,size=from.size(); boolean buffered=false;
                java.nio.ByteBuffer buffer=null;
                while(offset<size) {
                    long copied=0;
                    if(!buffered) {
                        try { copied=from.transferTo(offset,Math.min(8L*1024*1024,size-offset),to); }
                        catch(UnsupportedOperationException error) { buffered=true; }
                        if(copied==0) buffered=true;
                    }
                    if(buffered) {
                        if(buffer==null) buffer=java.nio.ByteBuffer.allocate(256*1024);
                        buffer.clear();buffer.limit((int)Math.min(buffer.capacity(),size-offset));
                        int read=from.read(buffer,offset);
                        if(read<=0) throw new EOFException("Snapshot truncated");
                        buffer.flip();while(buffer.hasRemaining()) to.write(buffer);
                        copied=read;
                    }
                    offset+=copied;progress.addAndGet(copied);
                }
                output.flush();
            }
        }
    }
    private static void copyDirectory(Context context,Uri tree,File source,Uri target,java.util.concurrent.atomic.AtomicLong progress) throws Exception {
        File[] files=source.listFiles();if(files==null) throw new IOException("Cannot read staging directory");
        for(File file:files) {
            if(internal(file.getName())) continue;
            Uri uri=child(context,tree,target,file.getName());
            if(uri==null) uri=DocumentsContract.createDocument(context.getContentResolver(),target,file.isDirectory()?DocumentsContract.Document.MIME_TYPE_DIR:"application/octet-stream",file.getName());
            if(uri==null) throw new IOException("Cannot create destination entry");
            if(file.isDirectory()) copyDirectory(context,tree,file,uri,progress);
            else copyPublishedFile(context,file,uri,progress);
        }
    }
    static void cancel(Context context,File source) throws Exception {
        File journal=new File(source,".portalo-provider.json");
        if(!journal.exists()) return;
        JSONObject state=load(journal);
        if(state.optBoolean("published") || state.optBoolean("ready")) throw new IOException("Publication may have completed; resume to resolve before cancelling");
        Uri temporary=null;
        if(state.has("uri")) temporary=Uri.parse(state.getString("uri"));
        else if(state.optBoolean("creating")) {
            Uri tree=Uri.parse(state.getString("tree"));
            Uri parent=DocumentsContract.buildDocumentUriUsingTree(tree,DocumentsContract.getTreeDocumentId(tree));
            temporary=child(context,tree,parent,state.getString("temporaryName"));
        }
        if(temporary!=null && !DocumentsContract.deleteDocument(context.getContentResolver(),temporary)) throw new IOException("Cannot delete incomplete directory");
        if(!journal.delete()) throw new IOException("Cannot clear publication journal");
    }
}
