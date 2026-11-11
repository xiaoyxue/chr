//! Message framing for byte-stream transports such as the NativeSocket.
//!
//! Each frame is a 4-byte big-endian payload length followed by that many payload bytes.
//! The length counts payload bytes only. Readers reject frames above a configured limit
//! before allocating for them.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Default upper bound for one control message. SDP offers are a few kilobytes.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame of {len} bytes exceeds the {max}-byte limit")]
    TooLarge { len: usize, max: usize },
    #[error("stream ended inside a frame")]
    UnexpectedEof,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Reads one frame. Returns `Ok(None)` when the stream ends cleanly between frames.
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_len: usize,
) -> Result<Option<Vec<u8>>, FrameError> {
    let mut header = [0u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        let read = reader.read(&mut header[filled..]).await?;
        if read == 0 {
            return if filled == 0 {
                Ok(None)
            } else {
                Err(FrameError::UnexpectedEof)
            };
        }
        filled += read;
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > max_len {
        return Err(FrameError::TooLarge { len, max: max_len });
    }
    let mut payload = vec![0u8; len];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::UnexpectedEof => FrameError::UnexpectedEof,
            _ => FrameError::Io(error),
        })?;
    Ok(Some(payload))
}

/// Writes one frame and flushes it.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    payload: &[u8],
    max_len: usize,
) -> Result<(), FrameError> {
    if payload.len() > max_len {
        return Err(FrameError::TooLarge {
            len: payload.len(),
            max: max_len,
        });
    }
    let len = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge {
        len: payload.len(),
        max: u32::MAX as usize,
    })?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trips_frames_and_detects_clean_end() {
        let (mut client, mut server) = tokio::io::duplex(64);
        write_frame(&mut client, b"hello", 16).await.unwrap();
        write_frame(&mut client, b"", 16).await.unwrap();
        drop(client);
        assert_eq!(
            read_frame(&mut server, 16).await.unwrap().as_deref(),
            Some(&b"hello"[..])
        );
        assert_eq!(
            read_frame(&mut server, 16).await.unwrap().as_deref(),
            Some(&b""[..])
        );
        assert!(read_frame(&mut server, 16).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rejects_oversized_and_truncated_frames() {
        let (mut client, mut server) = tokio::io::duplex(64);
        client.write_all(&100u32.to_be_bytes()).await.unwrap();
        assert!(matches!(
            read_frame(&mut server, 16).await,
            Err(FrameError::TooLarge { len: 100, max: 16 })
        ));

        let (mut client, mut server) = tokio::io::duplex(64);
        client.write_all(&[0, 0, 0, 4, b'a']).await.unwrap();
        drop(client);
        assert!(matches!(
            read_frame(&mut server, 16).await,
            Err(FrameError::UnexpectedEof)
        ));

        let (mut client, _server) = tokio::io::duplex(64);
        assert!(matches!(
            write_frame(&mut client, &[0; 17], 16).await,
            Err(FrameError::TooLarge { .. })
        ));
    }
}
