package __package__.infrastructure.repository.sku;

import __package__.infrastructure.convert.sku.SkuExampleEntityConvert;
import __package__.infrastructure.entity.sku.SkuExampleEntity;
import __package__.infrastructure.mapper.sku.SkuExampleMapper;
import __package__.domain.model.sku.SkuDO;
import __package__.domain.repository.sku.SkuExampleRepository;
import org.springframework.beans.factory.annotation.Autowired;
import org.springframework.stereotype.Repository;

/**
 * SkuExampleRepositoryImpl
 *
 * @author weirenyan
 * @date 2024/5/8
 */
@Repository
public class SkuExampleRepositoryImpl implements SkuExampleRepository {

    @Autowired
    private SkuExampleEntityConvert skuExampleEntityConvert;
    @Autowired
    private SkuExampleMapper skuExampleMapper;

    /**
     * 获取SKU
     *
     * @param skuId
     * @return
     */
    @Override
    public SkuDO getSku(long skuId) {

        SkuExampleEntity entity = new SkuExampleEntity();
        // convert to model
        return skuExampleEntityConvert.convert(entity);
    }
}
