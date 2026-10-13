package org.bevyengine.portalo;

import android.content.Context;
import android.content.ContentResolver;
import android.content.UriPermission;
import android.database.Cursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract;
import java.io.IOException;
import java.util.HashMap;
import java.util.HashSet;
import java.util.Map;
import java.util.Set;
import java.util.UUID;

/** Blocking provider operations. Rust calls these on its blocking I/O pool.
 * Each transfer has its own document and descriptor; only create/commit/abort
 * are serialized, not payload writes. All document mutations target our URI.
 */
final class SafStorage {
    private final Context context;
    private final Map<Long, Session> sessions = new HashMap<>();
    private long nextId = 1;

    private static final class Session {
        final Uri tree;
        final String name;
        Uri document;
        ParcelFileDescriptor descriptor;
        Session(Uri tree, String name, Uri document, ParcelFileDescriptor descriptor) {
            this.tree = tree;
            this.name = name;
            this.document = document;
            this.descriptor = descriptor;
        }
    }

    SafStorage(Context context) { this.context = context; }
    private ContentResolver resolver() { return context.getContentResolver(); }

    private Uri requireTree(String value) throws IOException {
        Uri tree = Uri.parse(value);
        if (!"content".equals(tree.getScheme()) || !DocumentsContract.isTreeUri(tree)) {
            throw new IOException("接收目录不是有效的系统文件夹，请重新选择");
        }
        boolean authorized = false;
        for (UriPermission permission : resolver().getPersistedUriPermissions()) {
            if (permission.getUri().equals(tree) && permission.isReadPermission() && permission.isWritePermission()) {
                authorized = true;
                break;
            }
        }
        if (!authorized) throw new IOException("接收目录授权已失效，请在设置中重新选择接收文件夹");
        Uri root = root(tree);
        try (Cursor cursor = resolver().query(root, new String[] {
                DocumentsContract.Document.COLUMN_MIME_TYPE,
                DocumentsContract.Document.COLUMN_FLAGS }, null, null, null)) {
            if (cursor == null || !cursor.moveToFirst()
                    || !DocumentsContract.Document.MIME_TYPE_DIR.equals(cursor.getString(0))) {
                throw new IOException("接收文件夹已删除或不可用，请在设置中重新选择");
            }
            if ((cursor.getInt(1) & DocumentsContract.Document.FLAG_DIR_SUPPORTS_CREATE) == 0) {
                throw new IOException("所选接收文件夹不允许创建文件，请选择可写文件夹");
            }
        } catch (SecurityException error) {
            throw new IOException("接收目录权限不可用，请在设置中重新选择接收文件夹", error);
        }
        return tree;
    }

    private static Uri root(Uri tree) {
        return DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree));
    }

    String label(String value) throws IOException {
        Uri tree = requireTree(value);
        try (Cursor cursor = resolver().query(root(tree), new String[] {
                DocumentsContract.Document.COLUMN_DISPLAY_NAME }, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                String name = cursor.getString(0);
                return name == null || name.isEmpty() ? tree.toString() : name;
            }
        }
        throw new IOException("无法读取所选文件夹，请重新选择");
    }

    synchronized void validate(String tree) throws IOException {
        long id = begin(tree, "probe");
        try {
            Session session = sessions.get(id);
            try (ParcelFileDescriptor.AutoCloseOutputStream output = new ParcelFileDescriptor.AutoCloseOutputStream(
                    ParcelFileDescriptor.dup(session.descriptor.getFileDescriptor()))) {
                output.write(0);
            }
        } finally {
            abort(id);
        }
    }

    synchronized long begin(String value, String name) throws IOException {
        Uri tree = requireTree(value);
        Uri document = null;
        ParcelFileDescriptor descriptor = null;
        try {
            document = DocumentsContract.createDocument(resolver(), root(tree), "application/octet-stream",
                    ".portalo-" + UUID.randomUUID() + ".part");
            if (document == null) throw new IOException("无法在接收文件夹创建临时文件");
            try (Cursor cursor = resolver().query(document, new String[] {
                    DocumentsContract.Document.COLUMN_FLAGS }, null, null, null)) {
                int required = DocumentsContract.Document.FLAG_SUPPORTS_RENAME | DocumentsContract.Document.FLAG_SUPPORTS_DELETE;
                if (cursor == null || !cursor.moveToFirst() || (cursor.getInt(0) & required) != required) {
                    throw new IOException("此存储提供方不支持临时文件重命名和清理，请选择本地文件夹");
                }
            }
            descriptor = resolver().openFileDescriptor(document, "w");
            if (descriptor == null) throw new IOException("无法写入所选接收文件夹");
            long id = nextId++;
            sessions.put(id, new Session(tree, name, document, descriptor));
            return id;
        } catch (Exception error) {
            if (descriptor != null) try { descriptor.close(); } catch (IOException cleanup) { error.addSuppressed(cleanup); }
            if (document != null) try { delete(document); } catch (Exception cleanup) { error.addSuppressed(cleanup); }
            throw new IOException("无法准备接收文件，请检查目录授权和可用空间：" + error.getMessage(), error);
        }
    }

    synchronized int duplicateFd(long id) throws IOException {
        Session session = sessions.get(id);
        if (session == null || session.descriptor == null) throw new IOException("接收任务已关闭");
        // Rust owns this duplicated descriptor and closes it after flush.
        return ParcelFileDescriptor.dup(session.descriptor.getFileDescriptor()).detachFd();
    }

    synchronized void finish(long id) throws IOException {
        Session session = sessions.get(id);
        if (session == null) throw new IOException("接收任务已关闭");
        if (session.descriptor != null) {
            session.descriptor.checkError();
            session.descriptor.close();
            session.descriptor = null;
        }
        // Snapshot names once, not an O(n) provider query for every suffix.
        Set<String> names = new HashSet<>();
        Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(session.tree,
                DocumentsContract.getTreeDocumentId(session.tree));
        try (Cursor cursor = resolver().query(children, new String[] {
                DocumentsContract.Document.COLUMN_DISPLAY_NAME }, null, null, null)) {
            if (cursor == null) throw new IOException("无法读取接收文件夹，文件尚未完成保存");
            while (cursor.moveToNext()) names.add(cursor.getString(0));
        }
        String target = session.name;
        long suffix = 0;
        while (names.contains(target)) target = session.name + "." + (++suffix);
        Uri renamed = DocumentsContract.renameDocument(resolver(), session.document, target);
        if (renamed == null) throw new IOException("无法完成文件重命名，请检查目录权限");
        session.document = renamed;
        sessions.remove(id);
    }

    synchronized void abort(long id) throws IOException {
        Session session = sessions.remove(id);
        if (session == null) return;
        IOException failure = null;
        if (session.descriptor != null) {
            try { session.descriptor.close(); } catch (IOException error) { failure = error; }
        }
        try { delete(session.document); }
        catch (Exception error) { throw new IOException("无法清理未完成的接收文件：" + session.document, error); }
        if (failure != null) throw failure;
    }

    private void delete(Uri document) throws IOException {
        if (!DocumentsContract.deleteDocument(resolver(), document)) throw new IOException("存储提供方未删除临时文件");
    }

    synchronized void abortAll() {
        for (Long id : new HashSet<>(sessions.keySet())) {
            try { abort(id); } catch (IOException error) { android.util.Log.e("Portalo", "Receive cleanup failed", error); }
        }
    }
}
