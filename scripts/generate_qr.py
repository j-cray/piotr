import sys
import qrcode
import io

def print_qr(data):
    qr = qrcode.QRCode(
        version=1,
        error_correction=qrcode.constants.ERROR_CORRECT_L,
        box_size=10,
        border=4,
    )
    qr.add_data(data)
    qr.make(fit=True)

    qr.print_ascii()

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python3 generate_qr.py <URI>")
        sys.exit(1)

    uri = sys.argv[1]
    print_qr(uri)
