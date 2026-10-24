package org.bevyengine.portalo;

import android.content.Context;
import android.content.ContentResolver;
import android.content.UriPermission;
import android.database.Cursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract;
import java.io.IOException;
import java.util.UUID;

/** Receive-folder selection and capability validation. Transfers use
 * TransferDocuments/DirectTransferStorage and their durable task journals. */
final class SafStorage {
    private final Context context;
    private volatile boolean closed;

    SafStorage(Context context) { this.context = context; }
    private ContentResolver resolver() { return context.getContentResolver(); }

    private void requireOpen() throws IOException {
        if (closed) throw new IOException("接收服务已关闭");
    }

    private Uri requireTree(String value) throws IOException {
        requireOpen();
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
        try (Cursor cursor = resolver().query(root(tree), new String[] {
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

    void validate(String value) throws IOException {
        Uri tree = requireTree(value);
        Uri document = null;
        IOException failure = null;
        try {
            requireOpen();
            document = DocumentsContract.createDocument(resolver(), root(tree), "application/octet-stream",
                    ".portalo-" + UUID.randomUUID() + ".part");
            if (document == null) throw new IOException("无法在接收文件夹创建临时文件");
            requireOpen();
            try (Cursor cursor = resolver().query(document, new String[] {
                    DocumentsContract.Document.COLUMN_FLAGS }, null, null, null)) {
                int required = DocumentsContract.Document.FLAG_SUPPORTS_RENAME | DocumentsContract.Document.FLAG_SUPPORTS_DELETE;
                if (cursor == null || !cursor.moveToFirst() || (cursor.getInt(0) & required) != required) {
                    throw new IOException("此存储提供方不支持临时文件重命名和清理，请选择本地文件夹");
                }
            }
            ParcelFileDescriptor descriptor = resolver().openFileDescriptor(document, "w");
            if (descriptor == null) throw new IOException("无法写入所选接收文件夹");
            try (ParcelFileDescriptor.AutoCloseOutputStream output = new ParcelFileDescriptor.AutoCloseOutputStream(descriptor)) {
                requireOpen();
                output.write(0);
            }
        } catch (Exception error) {
            failure = new IOException("无法验证接收文件夹：" + error.getMessage(), error);
            throw failure;
        } finally {
            // This validation worker owns its probe even after Activity shutdown.
            // Never wait for a provider or delete its document on the UI thread.
            if (document != null) {
                try {
                    if (!DocumentsContract.deleteDocument(resolver(), document)) {
                        throw new IOException("存储提供方未删除验证临时文件");
                    }
                } catch (Exception error) {
                    if (failure != null) failure.addSuppressed(error);
                    else throw new IOException("无法清理验证临时文件：" + document, error);
                }
            }
        }
    }

    void close() { closed = true; }
}
