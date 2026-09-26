export class TesseraClient {
    constructor(private baseUrl: string) {}
    
    async getAssets() {
        const res = await fetch(${this.baseUrl}/v1/assets);
        return res.json();
    }
}
