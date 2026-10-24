package org.bevyengine.portalo;

import android.content.Context;
import android.content.res.AssetFileDescriptor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.system.Os;
import android.system.OsConstants;
import android.system.StructStat;
import java.io.IOException;

/** Reopen a granted, seekable document without copying its contents. */
final class SendDocuments {
    static int open(Context context, Uri uri, long offset, long size) throws Exception {
        if (offset < 0 || size < 0) throw new IOException("Invalid document region");
        try (AssetFileDescriptor asset = context.getContentResolver().openAssetFileDescriptor(uri, "r")) {
            if (asset == null) throw new IOException("Cannot reopen document; check access permission");
            StructStat stat = Os.fstat(asset.getFileDescriptor());
            if (!OsConstants.S_ISREG(stat.st_mode) || offset > stat.st_size
                    || size > stat.st_size - offset || asset.getStartOffset() != offset
                    || asset.getLength() != size) {
                throw new IOException("Document region changed or no longer seekable");
            }
            Os.lseek(asset.getFileDescriptor(), offset, OsConstants.SEEK_SET);
            try (ParcelFileDescriptor copy = ParcelFileDescriptor.dup(asset.getFileDescriptor())) {
                return copy.detachFd();
            }
        }
    }
}
