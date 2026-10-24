package org.bevyengine.portalo;

import android.os.Bundle;
import android.content.Intent;
import android.content.Context;
import android.database.Cursor;
import android.net.Uri;
import android.net.wifi.WifiManager;
import android.provider.OpenableColumns;
import android.content.res.AssetFileDescriptor;
import android.os.ParcelFileDescriptor;
import android.system.Os;
import android.system.OsConstants;
import android.system.StructStat;
import java.io.File;
import java.io.InputStream;
import java.io.FileOutputStream;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

import com.google.androidgamesdk.GameActivity;

public class MainActivity extends GameActivity {
    private WifiManager.MulticastLock multicastLock;
    private final ExecutorService fileWorker = Executors.newSingleThreadExecutor();
    // Bound blocked provider reads as well as queued work. Cancellation signals
    // are advisory; a provider is allowed to ignore them.
    private final ExecutorService preparations = new java.util.concurrent.ThreadPoolExecutor(
            0, 2, 30, java.util.concurrent.TimeUnit.SECONDS,
            new java.util.concurrent.SynchronousQueue<>());
    private final android.os.Handler preparationUi = new android.os.Handler(android.os.Looper.getMainLooper());
    private Preparation preparation;
    static final class Preparation {
        final android.os.CancellationSignal signal = new android.os.CancellationSignal();
        volatile long bytes;
        volatile long advanced = android.os.SystemClock.elapsedRealtime();
        boolean done;
        android.app.ProgressDialog dialog;
        synchronized void check() throws java.io.IOException {
            if (done) throw new java.io.IOException("文件准备已取消");
        }
    }

    // CancellationSignal covers provider query/open, not reads on an already
    // opened pipe. Poll a nonblocking pipe/socket in bounded intervals instead.
    static void copyToCache(Preparation job, AssetFileDescriptor asset,
                            FileOutputStream writer) throws Exception {
        if (asset == null) throw new java.io.IOException("Cannot open selected file");
        java.io.FileDescriptor fd = asset.getFileDescriptor();
        StructStat stat = Os.fstat(fd);
        boolean pollable = OsConstants.S_ISFIFO(stat.st_mode) || OsConstants.S_ISSOCK(stat.st_mode);
        if (pollable) {
            int flags = Os.fcntlInt(fd, OsConstants.F_GETFL, 0);
            Os.fcntlInt(fd, OsConstants.F_SETFL, flags | OsConstants.O_NONBLOCK);
        }
        try (InputStream input = asset.createInputStream()) {
            byte[] buffer = new byte[256 * 1024];
            android.system.StructPollfd poll = new android.system.StructPollfd();
            poll.fd = fd;
            poll.events = (short) OsConstants.POLLIN;
            android.system.StructPollfd[] waits = { poll };
            long remaining = asset.getDeclaredLength();
            while (remaining != 0) {
                job.check();
                int limit = remaining < 0 ? buffer.length : (int) Math.min(remaining, buffer.length);
                int count;
                if (pollable) {
                    try {
                        if (Os.poll(waits, 100) == 0) continue;
                        job.check();
                        count = Os.read(fd, buffer, 0, limit);
                    } catch (android.system.ErrnoException error) {
                        if (error.errno == OsConstants.EINTR || error.errno == OsConstants.EAGAIN) continue;
                        throw error;
                    }
                    if (count == 0) break;
                } else {
                    count = input.read(buffer, 0, limit);
                    if (count == -1) break;
                }
                job.check();
                writer.write(buffer, 0, count);
                if (remaining > 0) remaining -= count;
                if (count > 0) {
                    job.bytes += count;
                    job.advanced = android.os.SystemClock.elapsedRealtime();
                }
            }
            if (remaining > 0) throw new java.io.EOFException("Document shortened during preparation");
        }
    }

    private void cancelPreparation(Preparation job) {
        synchronized (job) {
            if (job.done) return;
            job.done = true;
            nativeFilePicked("", "");
        }
        picking = false;
        if (preparation == job) preparation = null;
        job.dialog.dismiss();
        // A provider's cancellation callback may block too; keep it off UI.
        new Thread(job.signal::cancel, "provider-cancel").start();
    }

    private void refreshPreparation(Preparation job) {
        synchronized (job) {
            if (job.done) { job.dialog.dismiss(); return; }
        }
        if (android.os.SystemClock.elapsedRealtime() - job.advanced >= 30_000) {
            cancelPreparation(job);
            android.widget.Toast.makeText(this, "文件准备超时，请检查存储提供方", android.widget.Toast.LENGTH_LONG).show();
            return;
        }
        job.dialog.setMessage("已准备 " + (job.bytes / 1024) + " KiB");
        preparationUi.postDelayed(() -> refreshPreparation(job), 250);
    }
    private boolean picking;
    private final SafStorage receiveStorage = new SafStorage(this);
    private native void nativeFilePicked(String path, String name);
    private native void nativeFileDescriptorPicked(int fd, String name, long size, long offset, String uri);

    public int reopenSendDocument(String value, long offset, long size) throws Exception {
        return SendDocuments.open(this, Uri.parse(value), offset, size);
    }
    private native void nativeBackRequested();
    private native void nativeInsetsChanged(float top, float bottom);
    private native void nativeDirectoryPicked(String uri, String label, String error);

    public void validateReceiveTree(String uri) throws java.io.IOException { receiveStorage.validate(uri); }
    public long beginReceiveDocument(String uri, String name) throws java.io.IOException { return receiveStorage.begin(uri, name); }
    public int receiveDocumentFd(long id) throws java.io.IOException { return receiveStorage.duplicateFd(id); }
    public void finishReceiveDocument(long id) throws java.io.IOException { receiveStorage.finish(id); }
    public void abortReceiveDocument(long id) throws java.io.IOException { receiveStorage.abort(id); }
    public String publishTransfer(String tree, String path, String name, String id) throws Exception {
        return TransferDocuments.publish(this, tree, new File(path), name, id);
    }
    public long transferSaveProgress(String id) { return TransferDocuments.progress(id); }
    public String prepareTransferTarget(String tree, String state, String manifest) throws Exception {
        return DirectTransferStorage.prepare(this, tree, new File(state), manifest);
    }
    public int openTransferDocument(String uri, boolean write) throws Exception {
        return DirectTransferStorage.open(this, Uri.parse(uri), write);
    }
    public String publishTransferTarget(String state) throws Exception {
        return DirectTransferStorage.publish(this, new File(state));
    }
    public void cancelTransferTarget(String state) throws Exception {
        DirectTransferStorage.cancel(this, new File(state));
    }
    public void cancelTransfer(String path) throws Exception { TransferDocuments.cancel(this, new File(path)); }

    public void pickSendFolder() {
        runOnUiThread(() -> {
            if (picking) return;
            picking=true;
            try { startActivityForResult(new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE),1003); }
            catch (Exception error) { picking=false; nativeFilePicked("",""); }
        });
    }

    public void pickReceiveDirectory() {
        runOnUiThread(() -> {
            if (picking) { nativeDirectoryPicked("", "", "已有文件选择操作，请稍后重试"); return; }
            picking = true;
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                    | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION | Intent.FLAG_GRANT_PREFIX_URI_PERMISSION);
            try { startActivityForResult(intent, 1002); }
            catch (Exception error) { picking = false; nativeDirectoryPicked("", "", "无法打开系统文件夹选择器：" + error.getMessage()); }
        });
    }

    public void copyText(String text) {
        runOnUiThread(() -> {
            android.content.ClipboardManager manager = (android.content.ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
            if (manager != null) manager.setPrimaryClip(android.content.ClipData.newPlainText("Portalo", text));
        });
    }

    public void backgroundApp() { runOnUiThread(() -> moveTaskToBack(true)); }

    @Override
    public void onBackPressed() { nativeBackRequested(); }

    // GameActivity's input connection consumes BACK before onBackPressed on
    // some devices. Intercept it before forwarding keys to the native surface.
    @Override
    public boolean dispatchKeyEvent(android.view.KeyEvent event) {
        if (event.getKeyCode() == android.view.KeyEvent.KEYCODE_BACK) {
            if (event.getAction() == android.view.KeyEvent.ACTION_UP && !event.isCanceled()) nativeBackRequested();
            return true;
        }
        return super.dispatchKeyEvent(event);
    }

    public synchronized String getDeviceName() {
        android.content.SharedPreferences prefs = getPreferences(MODE_PRIVATE);
        String instance = prefs.getString("portalo_instance", null);
        if (instance == null) {
            instance = java.util.UUID.randomUUID().toString().substring(0, 8);
            prefs.edit().putString("portalo_instance", instance).apply();
        }
        return android.os.Build.MODEL + "-" + instance;
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        int surfaceColor = android.graphics.Color.rgb(245, 245, 242);
        getWindow().setStatusBarColor(surfaceColor);
        getWindow().setNavigationBarColor(surfaceColor);
        getWindow().getDecorView().setSystemUiVisibility(
                android.view.View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR
                        | android.view.View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR);
        getWindow().getDecorView().setOnApplyWindowInsetsListener((view, insets) -> {
            float density = getResources().getDisplayMetrics().density;
            nativeInsetsChanged(insets.getSystemWindowInsetTop() / density,
                    insets.getSystemWindowInsetBottom() / density);
            return view.onApplyWindowInsets(insets);
        });
        WifiManager wifi = (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
        if (wifi != null) {
            multicastLock = wifi.createMulticastLock("portalo-discovery");
            multicastLock.setReferenceCounted(false);
        }
    }

    @Override
    protected void onStart() {
        super.onStart();
        if (multicastLock != null && !multicastLock.isHeld()) multicastLock.acquire();
    }

    @Override
    protected void onStop() {
        if (multicastLock != null && multicastLock.isHeld()) multicastLock.release();
        super.onStop();
    }

    public void pickFile() {
        runOnUiThread(() -> {
            if (picking) { nativeFilePicked("", ""); return; }
            picking = true;
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType("*/*");
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
            startActivityForResult(intent, 1001);
        });
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request == 1002) {
            if (result != RESULT_OK || data == null || data.getData() == null) {
                picking = false; nativeDirectoryPicked("", "", ""); return;
            }
            Uri tree = data.getData();
            int flags = data.getFlags() & (Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
            fileWorker.execute(() -> {
                try {
                    getContentResolver().takePersistableUriPermission(tree, flags);
                    nativeDirectoryPicked(tree.toString(), receiveStorage.label(tree.toString()), "");
                } catch (Exception error) {
                    nativeDirectoryPicked("", "", "无法取得文件夹的长期读写权限，请重新选择：" + error.getMessage());
                } finally { runOnUiThread(() -> picking = false); }
            });
            return;
        }
        if (request != 1001 && request != 1003) return;
        if (result != RESULT_OK || data == null || (data.getData() == null && data.getClipData() == null)) { picking = false; nativeFilePicked("", ""); return; }
        Uri uri = data.getData() != null ? data.getData()
                : data.getClipData().getItemAt(0).getUri();
        Preparation job = new Preparation();
        preparation = job;
        job.dialog = new android.app.ProgressDialog(this);
        job.dialog.setTitle("正在准备文件");
        job.dialog.setMessage("正在读取文档…");
        job.dialog.setIndeterminate(true);
        job.dialog.setCancelable(true);
        job.dialog.setOnCancelListener(dialog -> cancelPreparation(job));
        job.dialog.setButton(android.content.DialogInterface.BUTTON_NEGATIVE, "取消",
                (dialog, which) -> cancelPreparation(job));
        job.dialog.show();
        refreshPreparation(job);
        try { preparations.execute(() -> {
            File output = null;
            try {
                if (request == 1003 || (data.getClipData() != null && data.getClipData().getItemCount() > 1)) {
                    output = new File(getCacheDir(),"outgoing/" + java.util.UUID.randomUUID());
                    if (!output.mkdirs()) throw new java.io.IOException("Cannot create snapshot folder");
                    String label;
                    if (request == 1003) {
                        label=TransferDocuments.exportTree(this,uri,output,job);
                    } else {
                        label="批量文件";
                        TransferDocuments.exportFiles(this,data.getClipData(),output,job);
                    }
                    synchronized (job) {
                        job.check(); nativeFilePicked(output.getAbsolutePath(),label); job.done=true;
                    }
                    return;
                }
                String name = "selected-file";
                job.check();
                try (Cursor cursor = getContentResolver().query(uri, new String[] { OpenableColumns.DISPLAY_NAME }, null, null, null, job.signal)) {
                    if (cursor != null && cursor.moveToFirst()) {
                        int index = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                        if (index >= 0 && !cursor.isNull(index)) name = cursor.getString(index);
                    }
                }
                name = name.replace('/', '_').replace('\\', '_');
                if (name.isEmpty() || name.equals(".") || name.equals("..")) name = "selected-file";
                // Probe on the file worker: SAF may expose regular files, slices,
                // pipes or remote streams. Only a bounded, known-size regular
                // file is handed directly to Rust; all others keep the cache path.
                ParcelFileDescriptor direct = null;
                long length = -1;
                long offset = -1;
                try (AssetFileDescriptor asset = getContentResolver().openAssetFileDescriptor(uri, "r", job.signal)) {
                    if (asset != null) {
                        StructStat stat = Os.fstat(asset.getFileDescriptor());
                        length = asset.getLength();
                        offset = asset.getStartOffset();
                        if (OsConstants.S_ISREG(stat.st_mode) && offset >= 0 && offset <= stat.st_size
                                && length >= 0 && length <= stat.st_size - offset) {
                            Os.lseek(asset.getFileDescriptor(), offset, OsConstants.SEEK_SET);
                            direct = ParcelFileDescriptor.dup(asset.getFileDescriptor());
                        }
                    }
                } catch (Exception error) {
                    if (direct != null) { try { direct.close(); } catch (Exception ignored) {} }
                    direct = null;
                    android.util.Log.d("Portalo", "Direct document unavailable; using cache", error);
                }
                if (direct != null) {
                    String reopenUri = "";
                    try {
                        getContentResolver().takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION);
                        int reopened = reopenSendDocument(uri.toString(), offset, length);
                        ParcelFileDescriptor.adoptFd(reopened).close();
                        reopenUri = uri.toString();
                    } catch (Exception error) {
                        android.util.Log.i("Portalo", "Document cannot be persistently reopened; capturing before send", error);
                    }
                    try (ParcelFileDescriptor owned = direct) {
                        synchronized (job) {
                            job.check();
                            nativeFileDescriptorPicked(owned.detachFd(), name, length, offset, reopenUri);
                            job.done = true;
                        }
                    }
                    android.util.Log.i("Portalo", "Selected document FD directly: " + length + " bytes");
                    return;
                }
                job.check();
                File directory = new File(getCacheDir(), "outgoing");
                if (!directory.isDirectory() && !directory.mkdirs()) throw new java.io.IOException("Cannot create outgoing cache");
                output = File.createTempFile("portalo-", ".part", directory);
                try (AssetFileDescriptor cached = getContentResolver().openAssetFileDescriptor(uri, "r", job.signal);
                     FileOutputStream writer = new FileOutputStream(output)) {
                    copyToCache(job, cached, writer);
                }
                synchronized (job) {
                    job.check();
                    nativeFilePicked(output.getAbsolutePath(), name);
                    job.done = true;
                }
            } catch (Exception error) {
                if (output != null) TransferDocuments.deleteLocal(output);
                android.util.Log.e("Portalo", "File selection failed", error);
                synchronized (job) {
                    if (!job.done) {
                        job.done = true;
                        nativeFilePicked("", "");
                        runOnUiThread(() -> android.widget.Toast.makeText(this, "无法读取所选文件，请检查访问权限和剩余空间", android.widget.Toast.LENGTH_LONG).show());
                    }
                }
            } finally { runOnUiThread(() -> {
                job.dialog.dismiss();
                if (preparation == job) { preparation = null; picking = false; }
            }); }
        }); } catch (java.util.concurrent.RejectedExecutionException error) {
            cancelPreparation(job);
            android.widget.Toast.makeText(this, "存储提供方仍有未结束的读取，请稍后重试", android.widget.Toast.LENGTH_LONG).show();
        }
    }

    @Override
    protected void onDestroy() {
        if (preparation != null) cancelPreparation(preparation);
        preparationUi.removeCallbacksAndMessages(null);
        preparations.shutdown();
        if (multicastLock != null && multicastLock.isHeld()) multicastLock.release();
        receiveStorage.abortAll();
        fileWorker.shutdown();
        super.onDestroy();
    }
    static {
        System.loadLibrary("portalo");
    }

}
